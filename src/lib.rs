//! The spelling checker: it reads a Hunspell dictionary (an `.aff` of rules and a `.dic` of
//! stems with their rule flags) and says whether a word is spelled right and, if not, what the
//! person probably meant.
//!
//! It implements exactly what the shipped English dictionaries (SCOWL's, as the npm packages
//! `dictionary-en` and `dictionary-en-gb` publish them; copied under `dictionaries/`) use, and refuses
//! an `.aff` that asks for more rather than quietly checking it wrong:
//! - UTF-8 text (`SET UTF-8`) with single-character flags (no `FLAG` line);
//! - `PFX`/`SFX` with strip, add and condition, a prefix and a suffix combining when both allow
//!   it (cross product), no affix carrying flags of its own;
//! - `ICONV` (the typographic apostrophe read as `'`), `NOSUGGEST`, and for suggestions `REP`,
//!   `KEY` and `NOSPLITSUGS`;
//! - `COMPOUNDRULE`, `COMPOUNDMIN` and `ONLYINCOMPOUND`, which these files use only for ordinals
//!   ("21st", "112th"), so compounding is only tried on a word that holds a digit;
//! - read past, as they tune Hunspell's own search, which this one replaces: `TRY`,
//!   `MAXNGRAMSUGS`, `MAXDIFF`, `ONLYMAXDIFF`; and `WORDCHARS`, as the client splits text into
//!   words before it asks.
//!
//! Every affix is applied once, at load, so the dictionary is held as its word forms (about
//! 124,000 for English): checking a word is one hash lookup.
//!
//! Capitals follow Hunspell: a word the dictionary has in lower case is also right Capitalised
//! or in ALL CAPS, and one it has capitalised ("Paris") is right in ALL CAPS but not lower case.
//! A hyphenated word is right when each part is. Any other word holding a digit ("x86", "2024")
//! is not a word the dictionary can judge, so it passes.
//!
//! Suggestions gather candidates three ways and rank them all by one cost:
//! - near spellings: each form carries a 64-bit signature (which letters it has, a hash of which
//!   letter pairs, its length), and the forms whose signatures differ from the word's in the
//!   fewest bits are kept, found by scanning them all (an XOR and a bit count each);
//! - sound-alikes: each form's sounds-like key (`sound_key`, a small English reduction in the
//!   spirit of Metaphone), the forms whose key is the word's or one edit from it, found with the
//!   same signatures over the keys;
//! - the `.aff`'s listed common misspellings (`REP`) applied where they occur, and the word split
//!   in two by a missing space.
//!
//! The cost (`slip_cost`) is a weighted edit distance: a missing letter, a vowel for a vowel, a
//! doubled letter, a swap, a neighbouring key and a listed misspelling cost less than an
//! unrelated letter, and a wrong first letter costs more; a sound-alike costs at most a set
//! amount. Against the public misspelling corpora this puts the right word first more often than
//! Hunspell does (the README has the measured table and how to reproduce it).

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

/// The most suggestions one misspelled word offers: 5, what Chromium's spellchecker shows
/// (`spellcheck::kMaxSuggestions`) and about what Firefox's menu lists; more is a list to read,
/// not a correction to pick.
pub const MAX_SUGGESTIONS: usize = 5;

//the farthest apart two letters may be swapped, or one letter moved, as a slip: 4 places,
//Hunspell's MAX_CHAR_DISTANCE
const SLIP_DISTANCE_MAX: usize = 4;

//the keyboard a wrong key is looked for on when the .aff gives no KEY: Hunspell's default, the
//three letter rows of a QWERTY keyboard with `|` between the rows
const DEFAULT_KEYBOARD: &str = "qwertyuiop|asdfghjkl|zxcvbnm";

//the costs a candidate is ranked by, in hundredths of an edit. An unrelated wrong, extra or
//missing letter would be a whole edit; the cheaper slips are the common ones. Tuned by hand on
//three of the public misspelling corpora (Norvig's first set, Aspell's, Wikipedia's) and checked
//on three held out (Norvig's second, Birkbeck, Holbrook), which improved alike; the README has
//the table and the command. The biggest single gain was a missing letter costing about half an
//edit: leaving a letter out is the commonest misspelling. A split ("to the" for "tothe") costs
//two edits, so a word within two edits comes first: the corpora's wrong splits ("co lour", "col
//our") far outnumber the ones they want
const WRONG_LETTER_COST: u32 = 100;
const EXTRA_LETTER_COST: u32 = 100;
const MISSING_LETTER_COST: u32 = 55;
const NEIGHBOUR_KEY_COST: u32 = 70;
const VOWEL_COST: u32 = 60;
const DOUBLED_LETTER_COST: u32 = 40;
const SWAP_COST: u32 = 50;
const MOVE_COST: u32 = 80;
const CONFUSION_COST: u32 = 60;
const FIRST_LETTER_COST: u32 = 50;
const SPLIT_COST: u32 = 200;
//a little, to break ties: a name offered for a word typed in lower case, an added apostrophe
//("potato's" for "potatos")
const NAME_COST: u32 = 10;
const APOSTROPHE_COST: u32 = 5;
//the letters a vowel-for-vowel slip is cheap between
const VOWELS: &str = "aeiouy";

//a sound-alike's cost: two edits when its sounds-like key is the word's, one more for each edit
//between the keys; at two it fills the list where spelling finds little, and seldom takes first
//place from a near spelling (at 1.5 it took first place from the right word more often than it
//gave first place to it)
const SOUND_SAME_COST: u32 = 200;
const SOUND_EDIT_COST: u32 = 100;
//a key of fewer than 4 sounds is matched only exactly, as one edit to a short key reaches
//hundreds of words; and at most 200 forms are taken from the keys
const SOUND_KEY_FUZZY_LENGTH: usize = 4;
const SOUND_FORMS_MOST: usize = 200;
//one edit to a key changes at most 7 bits of its signature: 2 letters, 4 letter pairs and the
//length class, so the keys within one edit are among those within 7 bits
const KEY_EDIT_BITS: u32 = 7;

//the signature search: a signature within 11 bits of the word's is near (two slips change about
//that many), and the 1,000 nearest are scored exactly (300 lost about a point of top-5 accuracy
//on the corpora; scoring them is most of a suggestion's time)
const SIGNATURE_RADIUS: u32 = 11;
const SIGNATURE_SCORED: usize = 1000;

//the forms index has at least 2 slots per form, so it stays under half full and a lookup probes
//about one slot; FNV-1a's published 64-bit offset basis and prime hash the forms
const INDEX_SLOTS_PER_ENTRY: usize = 2;
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

//what a word form may be used for: only judging text (NOSUGGEST), or suggesting too
const CHECK_ONLY: u8 = 1;
const SUGGESTIBLE: u8 = 2;

/// A loaded dictionary: every word form it allows, and what finds and ranks suggestions.
pub struct Dictionary {
    //every word form, its affixes applied: the forms' text end to end, where each starts (one
    //more start after the last), what each may be used for, and a hash table of form numbers
    //plus one (0 an empty slot) to find a form by its text
    form_text: String,
    form_starts: Vec<u32>,
    form_kinds: Vec<u8>,
    form_index: Vec<u32>,
    //what finds suggestions, built on the first one asked for, so a dictionary used only to
    //check words is ready sooner and smaller
    suggester: OnceLock<Suggester>,
    //the suffixes' added ends, a split leaving one of which is a fragment ("post ion")
    suffix_ends: Vec<String>,
    //what a word's characters are replaced by before it is looked up, such as ’ by '
    input_conversions: Vec<(String, String)>,
    //common misspellings as (wrong, right) pieces, and the same as characters for ranking
    replacements: Vec<(String, String)>,
    confusions: Vec<(Vec<char>, Vec<char>)>,
    //the keyboard rows a wrong key is looked for on, `|` between rows
    keyboard: Vec<char>,
    //NOSPLITSUGS: never suggest splitting a word in two
    no_split: bool,
    //the stems with capitals inside them ("McKinley"), in ALL CAPS, which is right too
    mixed_in_capitals: HashSet<String>,
    //the stems holding a digit, with their flags, which the ordinal compound rules join
    digit_stems: HashMap<String, Vec<char>>,
    compound_rules: Vec<Vec<RulePart>>,
    compound_min: usize,
}

impl Dictionary {
    /// Read a dictionary from its two files' text.
    /// input: the `.aff` and the `.dic` as UTF-8 text
    /// output: the dictionary, or a message naming the first rule this checker does not implement
    pub fn parse(aff: &str, dic: &str) -> Result<Dictionary, String> {
        let mut prefixes: Vec<Affix> = Vec::new();
        let mut suffixes: Vec<Affix> = Vec::new();
        let mut input_conversions = Vec::new();
        let mut replacements: Vec<(String, String)> = Vec::new();
        let mut keyboard: Vec<char> = DEFAULT_KEYBOARD.chars().collect();
        let mut no_split = false;
        let mut nosuggest: Option<char> = None;
        let mut only_in_compound: Option<char> = None;
        let mut compound_rules = Vec::new();
        let mut compound_min = 1;
        //a flag directive's value as its one character
        let flag = |fields: &[&str]| fields.get(1).and_then(|value| value.chars().next());
        //a table (ICONV, REP, COMPOUNDRULE) opens with a line giving its row count, and the rows
        //follow; this counts down the rows of the table open now
        let mut table_rows: HashMap<&str, usize> = HashMap::new();
        for line in aff.lines() {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let Some(&directive) = fields.first() else {
                continue;
            };
            let table = matches!(directive, "ICONV" | "REP" | "COMPOUNDRULE");
            match table_rows.get_mut(directive) {
                Some(rows) if *rows > 0 => *rows -= 1,
                _ if table && fields.len() == 2 => {
                    let rows = fields[1].parse().map_err(|_| line.to_string())?;
                    table_rows.insert(directive, rows);
                    continue;
                }
                _ => {}
            }
            match (directive, fields.len()) {
                (directive, _) if directive.starts_with('#') => {}
                ("SET", _) if fields.get(1) == Some(&"UTF-8") => {}
                ("WORDCHARS" | "TRY" | "MAXNGRAMSUGS" | "MAXDIFF" | "ONLYMAXDIFF", _) => {}
                ("KEY", 2) => keyboard = fields[1].chars().collect(),
                ("NOSPLITSUGS", 1) => no_split = true,
                ("NOSUGGEST", 2) => nosuggest = flag(&fields),
                ("ONLYINCOMPOUND", 2) => only_in_compound = flag(&fields),
                ("COMPOUNDMIN", 2) => {
                    compound_min = fields[1].parse().map_err(|_| line.to_string())?
                }
                ("ICONV", 3) => {
                    input_conversions.push((fields[1].to_string(), fields[2].to_string()))
                }
                //an underscore in a replacement stands for a space ("alot" to "a lot")
                ("REP", 3) => {
                    replacements.push((fields[1].to_string(), fields[2].replace('_', " ")))
                }
                ("COMPOUNDRULE", 2) => compound_rules.push(compound_rule(fields[1])?),
                ("PFX" | "SFX", 4) => {}
                ("PFX" | "SFX", 5) => {
                    let affix = affix(&fields, aff)?;
                    match directive {
                        "PFX" => prefixes.push(affix),
                        _ => suffixes.push(affix),
                    }
                }
                _ => return Err(format!("unsupported .aff rule: {line}")),
            }
        }

        //every word form: each stem with every affix its flags allow, as Hunspell's lookup would
        //accept them (a suffix, a prefix, or both when both combine). The first line of a .dic
        //is its count, every other a stem and its flags. The forms are written end to end into
        //one buffer, each as (start, end, kind), rather than a string apiece: there are about
        //150,000 before duplicates go
        let mut written = String::new();
        let mut spans: Vec<(u32, u32, u8)> = Vec::new();
        let mut suffixed = String::new();
        let mut mixed_in_capitals = HashSet::new();
        let mut digit_stems: HashMap<String, Vec<char>> = HashMap::new();
        for line in dic.lines().skip(1) {
            let entry = line.split('\t').next().unwrap_or("").trim();
            if entry.is_empty() {
                continue;
            }
            let (stem, flags) = entry.split_once('/').unwrap_or((entry, ""));
            let flags: Vec<char> = flags.chars().collect();
            if casing(stem) == Casing::Mixed {
                mixed_in_capitals.insert(stem.to_uppercase());
            }
            if stem.chars().any(|character| character.is_ascii_digit()) {
                digit_stems
                    .entry(stem.to_string())
                    .or_default()
                    .extend(&flags);
            }
            if only_in_compound.is_some_and(|flag| flags.contains(&flag)) {
                continue;
            }
            let kind = match nosuggest.is_some_and(|flag| flags.contains(&flag)) {
                true => CHECK_ONLY,
                false => SUGGESTIBLE,
            };
            //keep the form just written, or drop it when the affix did not apply
            let mut close = |written: &mut String, start: usize, made: bool| match made {
                true => spans.push((start as u32, written.len() as u32, kind)),
                false => written.truncate(start),
            };
            let start = written.len();
            written.push_str(stem);
            close(&mut written, start, true);
            for suffix in suffixes
                .iter()
                .filter(|suffix| flags.contains(&suffix.flag))
            {
                suffixed.clear();
                if !suffix.apply_suffix(stem, &mut suffixed) {
                    continue;
                }
                let start = written.len();
                written.push_str(&suffixed);
                close(&mut written, start, true);
                if suffix.cross_product {
                    for prefix in prefixes
                        .iter()
                        .filter(|prefix| prefix.cross_product && flags.contains(&prefix.flag))
                    {
                        let start = written.len();
                        let made = prefix.apply_prefix(&suffixed, &mut written);
                        close(&mut written, start, made);
                    }
                }
            }
            for prefix in prefixes
                .iter()
                .filter(|prefix| flags.contains(&prefix.flag))
            {
                let start = written.len();
                let made = prefix.apply_prefix(stem, &mut written);
                close(&mut written, start, made);
            }
        }
        //each form once, in the order first written, suggestible if any of the stems it grows
        //from is: a first hash table over the written forms finds the repeats
        let text = |span: &(u32, u32, u8)| &written[span.0 as usize..span.1 as usize];
        let slots = (spans.len() * INDEX_SLOTS_PER_ENTRY)
            .next_power_of_two()
            .max(1);
        let mut first_copy: Vec<u32> = vec![0; slots];
        let mut unique: Vec<(u32, u32, u8)> = Vec::with_capacity(spans.len());
        for span in &spans {
            let mut slot = text_hash(text(span)) & (slots - 1);
            loop {
                let entry = first_copy[slot];
                if entry == 0 {
                    first_copy[slot] = unique.len() as u32 + 1;
                    unique.push(*span);
                    break;
                }
                let kept = &mut unique[entry as usize - 1];
                if text(&*kept) == text(span) {
                    kept.2 = kept.2.max(span.2);
                    break;
                }
                slot = (slot + 1) & (slots - 1);
            }
        }
        drop((first_copy, spans));

        let mut dictionary = Dictionary {
            form_text: String::new(),
            form_starts: Vec::with_capacity(unique.len() + 1),
            form_kinds: Vec::with_capacity(unique.len()),
            form_index: Vec::new(),
            suggester: OnceLock::new(),
            suffix_ends: suffixes.iter().map(|suffix| suffix.add.clone()).collect(),
            confusions: replacements
                .iter()
                .map(|(wrong, right)| (wrong.chars().collect(), right.chars().collect()))
                .collect(),
            input_conversions,
            replacements,
            keyboard,
            no_split,
            mixed_in_capitals,
            digit_stems,
            compound_rules,
            compound_min,
        };
        for span in &unique {
            let start = dictionary.form_text.len() as u32;
            dictionary.form_starts.push(start);
            dictionary.form_text.push_str(text(span));
            dictionary.form_kinds.push(span.2);
        }
        let end = dictionary.form_text.len() as u32;
        dictionary.form_starts.push(end);
        dictionary.form_text.shrink_to_fit();
        drop((unique, written));

        //the hash table that finds a form by its text
        let slots = (dictionary.form_kinds.len() * INDEX_SLOTS_PER_ENTRY)
            .next_power_of_two()
            .max(1);
        dictionary.form_index = vec![0; slots];
        for id in 0..dictionary.form_kinds.len() {
            let mut slot = text_hash(dictionary.form(id)) & (slots - 1);
            while dictionary.form_index[slot] != 0 {
                slot = (slot + 1) & (slots - 1);
            }
            dictionary.form_index[slot] = id as u32 + 1;
        }

        Ok(dictionary)
    }

    //one form's text, by its number
    fn form(&self, id: usize) -> &str {
        &self.form_text[self.form_starts[id] as usize..self.form_starts[id + 1] as usize]
    }

    //what one exact form may be used for: 0 when the dictionary does not allow it
    fn kind_of(&self, word: &str) -> u8 {
        let mask = self.form_index.len() - 1;
        let mut slot = text_hash(word) & mask;
        loop {
            let entry = self.form_index[slot];
            if entry == 0 {
                return 0;
            }
            if self.form(entry as usize - 1) == word {
                return self.form_kinds[entry as usize - 1];
            }
            slot = (slot + 1) & mask;
        }
    }

    //the suggester: the suggestible forms, their signatures, and their sounds-like keys grouped
    fn suggester(&self) -> &Suggester {
        self.suggester.get_or_init(|| {
            let mut suggester = Suggester::default();
            //the suggestible forms' signatures, and their sounds-like keys: written end to end,
            //each as (its hash, start, end, position among the suggestible)
            let mut signatures = Vec::new();
            let mut keys_written: Vec<u8> = Vec::new();
            let mut keyed: Vec<(usize, u32, u32, u32)> = Vec::new();
            let mut lower: Vec<char> = Vec::new();
            for id in 0..self.form_kinds.len() {
                if self.form_kinds[id] != SUGGESTIBLE {
                    continue;
                }
                lower.clear();
                lower.extend(self.form(id).chars().flat_map(char::to_lowercase));
                let start = keys_written.len() as u32;
                keys_written.extend(sound_key(&lower));
                let position = suggester.suggestible.len() as u32;
                let end = keys_written.len() as u32;
                let hash = text_hash_bytes(&keys_written[start as usize..]);
                keyed.push((hash, start, end, position));
                signatures.push(signature(&lower));
                suggester.suggestible.push(id as u32);
            }
            suggester.spellings = Signatures(signatures);
            //grouped by sorting on each key's hash, which brings equal keys together without
            //comparing their text
            let key =
                |entry: &(usize, u32, u32, u32)| &keys_written[entry.1 as usize..entry.2 as usize];
            keyed.sort_unstable_by_key(|entry| (entry.0, entry.3));
            let mut key_signatures = Vec::new();
            for (at, entry) in keyed.iter().enumerate() {
                if at == 0 || key(&keyed[at - 1]) != key(entry) {
                    suggester.key_starts.push(suggester.key_text.len() as u32);
                    suggester
                        .key_text
                        .extend(key(entry).iter().map(|&sound| sound as char));
                    suggester
                        .key_form_starts
                        .push(suggester.key_forms.len() as u32);
                    key_signatures.push(signature(key(entry)));
                }
                suggester.key_forms.push(entry.3);
            }
            suggester.key_starts.push(suggester.key_text.len() as u32);
            suggester
                .key_form_starts
                .push(suggester.key_forms.len() as u32);
            suggester.sounds = Signatures(key_signatures);
            suggester
        })
    }

    //the respellings of one word with no hyphen, cheapest first, put back into the word's
    //capitals: near spellings, sound-alikes, listed misspellings and splits, ranked together
    fn respell(&self, word: &str, personal: &HashSet<String>) -> Vec<String> {
        let casing = casing(word);
        let as_typed: Vec<char> = word.chars().collect();
        let lower: Vec<char> = word.to_lowercase().chars().collect();
        //a word holding a digit is respelled only as an ordinal, its ending changed
        if lower.iter().any(|character| character.is_ascii_digit()) {
            let digits = word.trim_end_matches(|character: char| character.is_alphabetic());
            return ["st", "nd", "rd", "th"]
                .iter()
                .map(|ending| format!("{digits}{ending}"))
                .filter(|ordinal| ordinal != word && self.compound(ordinal))
                .collect();
        }
        let suggester = self.suggester();
        //every candidate, and what it costs as a sound-alike when it is one
        let mut candidates: Vec<Cow<str>> = Vec::new();
        let mut sound_costs: HashMap<&str, u32> = HashMap::new();

        //near spellings
        let near = suggester
            .spellings
            .near(signature(&lower), SIGNATURE_RADIUS, SIGNATURE_SCORED);
        for position in near {
            let form = self.form(suggester.suggestible[position as usize] as usize);
            candidates.push(Cow::Borrowed(form));
        }

        //sound-alikes: the forms whose sounds-like key is the word's, or one edit from it when
        //the key is long enough for one edit to mean something
        let typed_key = sound_key(&lower);
        let radius = u32::from(typed_key.len() >= SOUND_KEY_FUZZY_LENGTH);
        let farthest = if radius == 0 { 0 } else { KEY_EDIT_BITS };
        let mut keys: Vec<(u32, usize)> = suggester
            .sounds
            .near(signature(&typed_key), farthest, usize::MAX)
            .into_iter()
            .filter_map(|at| {
                let distance = edit_distance(suggester.key(at as usize), &typed_key);
                (distance <= radius).then_some((distance, at as usize))
            })
            .collect();
        keys.sort_unstable();
        let mut taken = 0;
        for (distance, at) in keys {
            let range =
                suggester.key_form_starts[at] as usize..suggester.key_form_starts[at + 1] as usize;
            for &position in &suggester.key_forms[range] {
                if taken == SOUND_FORMS_MOST {
                    break;
                }
                taken += 1;
                let form = self.form(suggester.suggestible[position as usize] as usize);
                let cost = SOUND_SAME_COST + distance * SOUND_EDIT_COST;
                let known = sound_costs.entry(form).or_insert(cost);
                *known = (*known).min(cost);
                candidates.push(Cow::Borrowed(form));
            }
        }

        //the listed common misspellings, at every place each occurs, overlaps included; one
        //holding a space ("alot" to "a lot") is good when each of its words is
        let text: String = lower.iter().collect();
        for (wrong, right) in &self.replacements {
            let mut from = 0;
            while let Some(offset) = text[from..].find(wrong.as_str()) {
                let at = from + offset;
                let replaced = format!("{}{right}{}", &text[..at], &text[at + wrong.len()..]);
                if replaced
                    .split(' ')
                    .all(|part| self.suggestible_word(part, personal))
                {
                    candidates.push(Cow::Owned(replaced));
                }
                from = at + wrong.chars().next().map_or(1, char::len_utf8);
            }
        }

        //a missing space, at every place where both halves are words. A half of one letter, or
        //a second half that is one of the language's own suffixes ("post ion", "admit ed"), is
        //a fragment of the word rather than a word, and is not offered
        if !self.no_split && lower.len() >= 4 {
            for at in 2..lower.len() - 1 {
                let first: String = lower[..at].iter().collect();
                let second: String = lower[at..].iter().collect();
                if self.suggestible_word(&first, personal)
                    && self.suggestible_word(&second, personal)
                    && !self.suffix_ends.contains(&second)
                {
                    candidates.push(Cow::Owned(format!("{first} {second}")));
                }
            }
        }

        //the person's own words are candidates like any other
        candidates.extend(personal.iter().map(|word| Cow::Borrowed(word.as_str())));

        //each once, ranked by its cost; equal costs keep the order found. The listed
        //misspellings in the word and the cost table are worked out once, not per candidate
        let confusions_at = self.confusions_in(&lower);
        let mut table: Vec<u32> = Vec::new();
        let mut candidate_lower: Vec<char> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut ranked: Vec<(u32, &str)> = Vec::new();
        for candidate in &candidates {
            if !seen.insert(candidate) {
                continue;
            }
            candidate_lower.clear();
            candidate_lower.extend(candidate.chars().flat_map(char::to_lowercase));
            let mut cost = self.slip_cost(&lower, &confusions_at, &candidate_lower, &mut table);
            if let Some(&sound) = sound_costs.get(candidate.as_ref()) {
                cost = cost.min(sound);
            }
            ranked.push((cost + ranking_extra(&as_typed, candidate), candidate));
        }
        ranked.sort_by_key(|(cost, _)| *cost);
        ranked
            .into_iter()
            .map(|(_, respelling)| match casing {
                Casing::Capitalised | Casing::AllCaps => recase(respelling, casing),
                Casing::Lower | Casing::Mixed => respelling.to_string(),
            })
            .collect()
    }

    //whether a word may be suggested exactly as written: the person's word, or a form not
    //marked never-to-suggest
    fn suggestible_word(&self, word: &str, personal: &HashSet<String>) -> bool {
        !word.is_empty() && (personal.contains(word) || self.kind_of(word) == SUGGESTIBLE)
    }

    //check a converted word; `nosuggest` says whether forms marked never-to-suggest count, which
    //they do when judging the text and do not when choosing what to suggest
    fn check_converted(&self, word: &str, personal: &HashSet<String>, nosuggest: bool) -> bool {
        if word.is_empty() {
            return false;
        }
        //a hyphenated word is right when each of its parts is
        if word.contains('-') {
            return word
                .split('-')
                .filter(|part| !part.is_empty())
                .all(|part| self.check_converted(part, personal, nosuggest));
        }
        //a word holding a digit is judged only when it is shaped like an ordinal
        if word.chars().any(|character| character.is_ascii_digit()) {
            let digits = word.trim_end_matches(|character: char| character.is_alphabetic());
            let ordinal = digits.len() < word.len()
                && digits.chars().all(|character| character.is_ascii_digit());
            return !ordinal || self.compound(&word.to_lowercase());
        }
        //the forms a word may be found under, by its capitals
        let lower = word.to_lowercase();
        let forms: Vec<String> = match casing(word) {
            Casing::Lower | Casing::Mixed => vec![word.to_string()],
            Casing::Capitalised => vec![word.to_string(), lower],
            Casing::AllCaps => vec![word.to_string(), recase(&lower, Casing::Capitalised), lower],
        };
        let least = if nosuggest { CHECK_ONLY } else { SUGGESTIBLE };
        forms
            .iter()
            .any(|form| personal.contains(form) || self.kind_of(form) >= least)
            || self.mixed_in_capitals.contains(word)
    }

    /// Whether a word is spelled right.
    /// input: one word as the text has it, and the person's own added words
    /// output: true when the dictionary or the person's words have it under one of its forms
    pub fn check(&self, word: &str, personal: &HashSet<String>) -> bool {
        let word = self.convert(word);
        self.check_converted(&word, personal, true)
    }

    /// What a misspelled word was probably meant to be.
    /// input: one word as the text has it, and the person's own added words
    /// output: at most `MAX_SUGGESTIONS` words that check, most likely first, in the word's own
    ///   capitals and with its own kind of apostrophe; empty when nothing close is found
    pub fn suggest(&self, word: &str, personal: &HashSet<String>) -> Vec<String> {
        let typographic = word.contains('’');
        let converted = self.convert(word);
        let respellings = match converted.contains('-') {
            true => self.respell_hyphenated(&converted, personal),
            false => self.respell(&converted, personal),
        };
        //each once, never the word itself, with the word's own kind of apostrophe
        let mut found: Vec<String> = Vec::new();
        for respelling in respellings {
            let respelling = match typographic {
                true => respelling.replace('\'', "’"),
                false => respelling,
            };
            if respelling != word && !found.contains(&respelling) {
                found.push(respelling);
            }
        }
        found.truncate(MAX_SUGGESTIONS);
        found
    }

    //a hyphenated word is respelled at its first wrong part, each respelling of that part put
    //back between the others ("wel-known" to "well-known")
    fn respell_hyphenated(&self, word: &str, personal: &HashSet<String>) -> Vec<String> {
        let parts: Vec<&str> = word.split('-').collect();
        let wrong = parts
            .iter()
            .position(|part| !part.is_empty() && !self.check_converted(part, personal, true));
        let Some(wrong) = wrong else {
            return Vec::new();
        };
        self.respell(parts[wrong], personal)
            .iter()
            .map(|respelling| {
                let mut whole = parts.clone();
                whole[wrong] = respelling;
                whole.join("-")
            })
            .collect()
    }

    //how costly a slip it is to type `candidate` as `typed`, both in lower case, in hundredths of
    //an edit: a weighted edit distance where a wrong key next to the right one, one vowel for
    //another, a doubled or undoubled letter, two letters swapped and a listed common misspelling
    //(REP) each cost less than an unrelated letter, and getting the first letter wrong costs more
    //input: the typed word, the listed misspellings found in it (`confusions_in`), the candidate,
    //and a buffer to reuse for the table
    fn slip_cost(
        &self,
        typed: &[char],
        confusions_at: &[(usize, usize)],
        candidate: &[char],
        cost: &mut Vec<u32>,
    ) -> u32 {
        let width = candidate.len() + 1;
        cost.clear();
        cost.resize((typed.len() + 1) * width, u32::MAX);
        cost[0] = 0;
        //each cell pushes its cost forward to the cells one slip on, so a cell is final by the
        //time the walk reaches it
        for at in 0..=typed.len() {
            for other in 0..=candidate.len() {
                let here = cost[at * width + other];
                if here == u32::MAX {
                    continue;
                }
                let mut relax = |to_at: usize, to_other: usize, step: u32| {
                    let cell = &mut cost[to_at * width + to_other];
                    *cell = (*cell).min(here + step);
                };
                //a letter typed that is not in the candidate (an extra one)
                if at < typed.len() {
                    relax(at + 1, other, letter_cost(typed, at, EXTRA_LETTER_COST));
                }
                //a letter of the candidate left out
                if other < candidate.len() {
                    relax(
                        at,
                        other + 1,
                        letter_cost(candidate, other, MISSING_LETTER_COST),
                    );
                }
                //the same letter, or a wrong one
                if at < typed.len() && other < candidate.len() {
                    let step = match typed[at] == candidate[other] {
                        true => 0,
                        false => self.wrong_letter_cost(typed[at], candidate[other]),
                    };
                    relax(at + 1, other + 1, step);
                }
                //two neighbouring letters swapped
                if at + 1 < typed.len()
                    && other + 1 < candidate.len()
                    && typed[at] == candidate[other + 1]
                    && typed[at + 1] == candidate[other]
                {
                    relax(at + 2, other + 2, SWAP_COST);
                }
                //a letter typed two or three places early or late (one place is a swap)
                for distance in 2..SLIP_DISTANCE_MAX {
                    if at + distance >= typed.len() || other + distance >= candidate.len() {
                        break;
                    }
                    let early = typed[at] == candidate[other + distance]
                        && typed[at + 1..=at + distance] == candidate[other..other + distance];
                    let late = candidate[other] == typed[at + distance]
                        && typed[at..at + distance] == candidate[other + 1..=other + distance];
                    if early || late {
                        relax(at + distance + 1, other + distance + 1, MOVE_COST);
                    }
                }
                //a common misspelling the .aff lists, such as "f" typed for "ph"
                for &(_, rule) in confusions_at.iter().filter(|(start, _)| *start == at) {
                    let (wrong, right) = &self.confusions[rule];
                    if candidate[other..].starts_with(right) {
                        //the whole word listed as a misspelling is the language's own answer
                        let whole = at == 0 && wrong.len() == typed.len();
                        let step = if whole { 0 } else { CONFUSION_COST };
                        relax(at + wrong.len(), other + right.len(), step);
                    }
                }
            }
        }
        let first_wrong = typed.first() != candidate.first();
        cost[typed.len() * width + candidate.len()]
            + if first_wrong { FIRST_LETTER_COST } else { 0 }
    }

    //the listed misspellings found in a typed word, as (where it starts, which rule), worked out
    //once per word rather than for every candidate
    fn confusions_in(&self, typed: &[char]) -> Vec<(usize, usize)> {
        (0..typed.len())
            .flat_map(|at| {
                self.confusions
                    .iter()
                    .enumerate()
                    .filter(move |(_, (wrong, _))| typed[at..].starts_with(wrong))
                    .map(move |(rule, _)| (at, rule))
            })
            .collect()
    }

    //what one letter typed in place of another costs: less for a key beside it or a vowel for a
    //vowel
    fn wrong_letter_cost(&self, typed: char, meant: char) -> u32 {
        //a space or hyphen is only ever a split, priced as one
        if [typed, meant]
            .iter()
            .any(|&letter| letter == ' ' || letter == '-')
        {
            return SPLIT_COST;
        }
        if VOWELS.contains(typed) && VOWELS.contains(meant) {
            return VOWEL_COST;
        }
        let beside = self.keyboard.windows(2).any(|pair| {
            (pair[0] == typed && pair[1] == meant) || (pair[0] == meant && pair[1] == typed)
        });
        match beside {
            true => NEIGHBOUR_KEY_COST,
            false => WRONG_LETTER_COST,
        }
    }

    //a word with its input conversions applied, such as ’ read as '
    fn convert(&self, word: &str) -> String {
        let mut converted = word.to_string();
        for (from, to) in &self.input_conversions {
            converted = converted.replace(from.as_str(), to);
        }
        converted
    }

    //whether a word splits into stems that one compound rule allows ("21st" is "2" then "1st")
    fn compound(&self, word: &str) -> bool {
        let characters: Vec<char> = word.chars().collect();
        self.compound_rules
            .iter()
            .any(|rule| self.compound_from(&characters, rule, 0))
    }

    //match the rest of a word against the rest of a rule, taking one stem at a time; a word needs
    //at least two stems to be a compound, which `taken` counts
    fn compound_from(&self, characters: &[char], rule: &[RulePart], taken: usize) -> bool {
        let Some((part, rest)) = rule.split_first() else {
            return characters.is_empty() && taken >= 2;
        };
        //a repeating part may also match nothing
        if part.repeats && self.compound_from(characters, rest, taken) {
            return true;
        }
        (self.compound_min..=characters.len()).any(|length| {
            let piece: String = characters[..length].iter().collect();
            let listed = self
                .digit_stems
                .get(&piece)
                .is_some_and(|flags| flags.contains(&part.flag));
            let next = if part.repeats { rule } else { rest };
            listed && self.compound_from(&characters[length..], next, taken + 1)
        })
    }
}

impl Affix {
    //write the word this suffix makes of a stem: the stripped end taken off and the added end
    //put on, when the stem is longer than what is stripped and its end meets the condition;
    //output: whether it made one
    fn apply_suffix(&self, stem: &str, out: &mut String) -> bool {
        let Some(base) = stem.strip_suffix(self.strip.as_str()) else {
            return false;
        };
        let fits = !base.is_empty() && self.end_fits(stem);
        if fits {
            out.push_str(base);
            out.push_str(&self.add);
        }
        fits
    }

    //write the word this prefix makes of a stem, its condition holding at the stem's start;
    //output: whether it made one
    fn apply_prefix(&self, stem: &str, out: &mut String) -> bool {
        let Some(base) = stem.strip_prefix(self.strip.as_str()) else {
            return false;
        };
        let fits = !base.is_empty() && self.start_fits(stem);
        if fits {
            out.push_str(&self.add);
            out.push_str(base);
        }
        fits
    }

    //whether a stem's last letters meet a suffix's condition, one class per letter
    fn end_fits(&self, stem: &str) -> bool {
        let mut tail = stem.chars().rev();
        self.condition
            .iter()
            .rev()
            .all(|class| tail.next().is_some_and(|letter| class.matches(letter)))
    }

    //whether a stem's first letters meet a prefix's condition
    fn start_fits(&self, stem: &str) -> bool {
        let mut head = stem.chars();
        self.condition
            .iter()
            .all(|class| head.next().is_some_and(|letter| class.matches(letter)))
    }
}

impl CharClass {
    fn matches(&self, character: char) -> bool {
        match self {
            CharClass::Any => true,
            CharClass::OneOf(set) => set.contains(&character),
            CharClass::NoneOf(set) => !set.contains(&character),
        }
    }
}

//one PFX or SFX entry line: `SFX D y ied [^aeiou]y`, whose header line (`SFX D Y 4`) earlier in
//the file says whether its flag combines with the other side
fn affix(fields: &[&str], aff: &str) -> Result<Affix, String> {
    let line = fields.join(" ");
    let flag = fields[1].chars().next().ok_or_else(|| line.clone())?;
    if fields[3].contains('/') {
        return Err(format!(
            "unsupported .aff rule (an affix with flags): {line}"
        ));
    }
    let header = aff.lines().find(|header| {
        let parts: Vec<&str> = header.split_whitespace().collect();
        parts.len() == 4 && parts[0] == fields[0] && parts[1] == fields[1]
    });
    let cross_product = header.is_some_and(|header| header.split_whitespace().nth(2) == Some("Y"));
    //a strip or add of 0 means nothing
    let nothing_if_zero = |field: &str| match field {
        "0" => String::new(),
        _ => field.to_string(),
    };
    Ok(Affix {
        flag,
        cross_product,
        strip: nothing_if_zero(fields[2]),
        add: nothing_if_zero(fields[3]),
        condition: condition(fields[4]).ok_or_else(|| line.clone())?,
    })
}

//an affix condition as one class per character position
fn condition(text: &str) -> Option<Vec<CharClass>> {
    if text == "." {
        return Some(Vec::new());
    }
    let mut classes = Vec::new();
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        classes.push(match character {
            '.' => CharClass::Any,
            '[' => {
                let set: String = characters.by_ref().take_while(|&c| c != ']').collect();
                match set.strip_prefix('^') {
                    Some(excluded) => CharClass::NoneOf(excluded.chars().collect()),
                    None => CharClass::OneOf(set.chars().collect()),
                }
            }
            _ => CharClass::OneOf(vec![character]),
        });
    }
    Some(classes)
}

//a compound rule such as `n*1t` as its parts; these files use only `*` between flags
fn compound_rule(text: &str) -> Result<Vec<RulePart>, String> {
    let mut parts: Vec<RulePart> = Vec::new();
    for character in text.chars() {
        match (character, parts.last_mut()) {
            ('*', Some(last)) => last.repeats = true,
            ('*' | '?' | '(' | ')', _) => {
                return Err(format!("unsupported .aff rule: COMPOUNDRULE {text}"));
            }
            (flag, _) => parts.push(RulePart {
                flag,
                repeats: false,
            }),
        }
    }
    Ok(parts)
}

//where a lower-case letter a-z sits in the alphabet, or None for any other character
fn alphabet_place(letter: char) -> Option<usize> {
    letter
        .is_ascii_lowercase()
        .then(|| (letter as u8 - b'a') as usize)
}

//a form's place in the index: FNV-1a over its bytes
fn text_hash(text: &str) -> usize {
    text_hash_bytes(text.as_bytes())
}

fn text_hash_bytes(bytes: &[u8]) -> usize {
    bytes.iter().fold(FNV_OFFSET_BASIS, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
    }) as usize
}

//what a letter typed extra, or left out, costs: less when it doubles the letter beside it
//("acomodate" for "accommodate"), and a space or hyphen costs a split's price
fn letter_cost(word: &[char], at: usize, plain: u32) -> u32 {
    let letter = word[at];
    if letter == ' ' || letter == '-' {
        return SPLIT_COST;
    }
    let doubled = (at > 0 && word[at - 1] == letter) || word.get(at + 1) == Some(&letter);
    match doubled {
        true => DOUBLED_LETTER_COST,
        false => plain,
    }
}

//how a word is capitalised, by its letters alone
fn casing(word: &str) -> Casing {
    let mut letters = word.chars().filter(|character| character.is_alphabetic());
    let Some(first) = letters.next() else {
        return Casing::Lower;
    };
    let rest: Vec<char> = letters.collect();
    let rest_upper = rest.iter().all(|character| character.is_uppercase());
    let rest_lower = rest.iter().all(|character| character.is_lowercase());
    match (first.is_uppercase(), rest_upper, rest_lower) {
        (false, _, true) => Casing::Lower,
        (true, true, _) if !rest.is_empty() => Casing::AllCaps,
        (true, _, true) => Casing::Capitalised,
        _ => Casing::Mixed,
    }
}

//a lower-case word put into the capitals of another
fn recase(word: &str, casing: Casing) -> String {
    match casing {
        Casing::AllCaps => word.to_uppercase(),
        Casing::Capitalised => {
            let mut characters = word.chars();
            characters
                .next()
                .map(|first| first.to_uppercase().chain(characters).collect())
                .unwrap_or_default()
        }
        Casing::Lower | Casing::Mixed => word.to_string(),
    }
}

//a sounds-like key: the word reduced to roughly how it sounds in English, so that spellings
//that sound alike ("sumthing", "something"; "becos", "because") share a key. Written here in
//the spirit of Metaphone (Philips 1990): silent and doubled letters dropped, letters that make
//the same sound folded together, vowels kept only at the start
fn sound_key(word: &[char]) -> Vec<u8> {
    let letters: Vec<char> = word
        .iter()
        .copied()
        .filter(|c| c.is_ascii_alphabetic())
        .collect();
    let at = |i: usize| letters.get(i).copied().unwrap_or(' ');
    let vowel = |c: char| "aeiouy".contains(c);
    let mut key: Vec<u8> = Vec::new();
    let mut i = 0;
    //silent first letters
    if matches!(
        (at(0), at(1)),
        ('k', 'n') | ('g', 'n') | ('p', 'n') | ('w', 'r') | ('p', 's')
    ) {
        i = 1;
    }
    while i < letters.len() {
        let (c, next) = (at(i), at(i + 1));
        let mut skip = 1;
        let sound: Option<char> = match c {
            'a' | 'e' | 'i' | 'o' | 'u' | 'y' => (i == 0).then_some('a'),
            'c' if next == 'h' => {
                skip = 2;
                Some('x')
            }
            'c' if next == 'k' => {
                skip = 2;
                Some('k')
            }
            'c' if matches!(next, 'e' | 'i' | 'y') => Some('s'),
            'c' | 'q' | 'k' => Some('k'),
            'g' if next == 'h' => {
                skip = 2;
                (i == 0).then_some('k')
            }
            'g' if matches!(next, 'e' | 'i' | 'y') => Some('j'),
            'g' => Some('k'),
            'd' if next == 'g' && matches!(at(i + 2), 'e' | 'i' | 'y') => {
                skip = 2;
                Some('j')
            }
            'p' if next == 'h' => {
                skip = 2;
                Some('f')
            }
            's' if next == 'h' => {
                skip = 2;
                Some('x')
            }
            's' | 't' if next == 'i' && matches!(at(i + 2), 'o' | 'a') => Some('x'),
            't' if next == 'h' => {
                skip = 2;
                Some('0')
            }
            'x' => {
                key.push(b'k');
                Some('s')
            }
            'z' => Some('s'),
            'w' | 'h' => {
                (vowel(next) && !(c == 'h' && i > 0 && "cgpst".contains(at(i - 1)))).then_some(c)
            }
            'v' => Some('f'),
            other => Some(other),
        };
        if let Some(sound) = sound
            && key.last() != Some(&(sound as u8))
        {
            key.push(sound as u8);
        }
        i += skip;
    }
    key
}

//the edit distance between two words: letters put in, taken out or changed, each one
//(Levenshtein 1966); used to compare sounds-like keys
fn edit_distance<T: PartialEq + Copy>(a: &[T], b: &[T]) -> u32 {
    let mut previous: Vec<u32> = (0..=b.len() as u32).collect();
    let mut row = vec![0; b.len() + 1];
    for (i, &x) in a.iter().enumerate() {
        row[0] = i as u32 + 1;
        for (j, &y) in b.iter().enumerate() {
            row[j + 1] = (previous[j] + u32::from(x != y))
                .min(previous[j + 1] + 1)
                .min(row[j] + 1);
        }
        std::mem::swap(&mut previous, &mut row);
    }
    previous[b.len()]
}

//a word's 64-bit signature: bits 0-25 say which of the letters a-z it holds, bits 26-57 which
//letter pairs (hashed into 32), and bits 58-63 its length as a thermometer code; a slip changes
//few bits, so words a slip or two apart have signatures a few bits apart
fn signature<T: Copy + Into<char>>(word: &[T]) -> u64 {
    let bit = |place: u64| 1u64 << place;
    let mut signature: u64 = 0;
    for &letter in word {
        if let Some(place) = alphabet_place(letter.into()) {
            signature |= bit(place as u64);
        }
    }
    for pair in word.windows(2) {
        let hash = (pair[0].into() as u64)
            .wrapping_mul(31)
            .wrapping_add(pair[1].into() as u64)
            % 32;
        signature |= bit(26 + hash);
    }
    //a thermometer code of the length: one more bit for each two letters past three
    for (place, length) in [4usize, 6, 8, 10, 12, 14].iter().enumerate() {
        if word.len() >= *length {
            signature |= bit(58 + place as u64);
        }
    }
    signature
}

//what finds suggestions: the forms that may be suggested, by number, and their signatures; the
//sounds-like keys of those forms end to end and where each starts, the forms under each key
//(positions in `suggestible`), and the keys' signatures
#[derive(Default)]
struct Suggester {
    suggestible: Vec<u32>,
    spellings: Signatures,
    key_text: String,
    key_starts: Vec<u32>,
    key_form_starts: Vec<u32>,
    key_forms: Vec<u32>,
    sounds: Signatures,
}

impl Suggester {
    //one sounds-like key, by its number, as bytes (keys are ASCII)
    fn key(&self, at: usize) -> &[u8] {
        &self.key_text.as_bytes()[self.key_starts[at] as usize..self.key_starts[at + 1] as usize]
    }
}

//the 64-bit signatures of a list of words. Finding the near ones is a scan of them all, an XOR
//and a bit count each: about 0.1 ms for English's 124,000. Multi-index hashing (Norouzi, Punjani
//& Fleet 2012; four tables of 16-bit chunks, probed within 2 bits) was measured against the scan
//on the same corpora: the same suggestions, no faster (2.8 ms against 2.6 ms a suggestion), and
//3.5 MiB more memory, so the scan stayed
#[derive(Default)]
struct Signatures(Vec<u64>);

impl Signatures {
    //the positions of the words whose signatures are within `farthest` bits of the query's,
    //nearest first (equal ones in order), at most `most` of them
    fn near(&self, query: u64, farthest: u32, most: usize) -> Vec<u32> {
        let mut near: Vec<(u32, u32)> = Vec::new();
        for (position, &signature) in self.0.iter().enumerate() {
            let distance = (signature ^ query).count_ones();
            if distance <= farthest {
                near.push((distance, position as u32));
            }
        }
        near.sort_unstable();
        near.truncate(most);
        near.into_iter().map(|(_, position)| position).collect()
    }
}

//what a candidate costs besides its slips: a name offered for a word typed in lower case, or an
//apostrophe the word did not have ("potato's" for "potatos"), costs a little, so that at equal
//slips the plain word comes first
fn ranking_extra(typed: &[char], candidate: &str) -> u32 {
    let typed_lower = typed.iter().all(|letter| !letter.is_uppercase());
    let name = typed_lower && candidate.chars().any(char::is_uppercase);
    let apostrophe = candidate.contains('\'') && !typed.contains(&'\'');
    u32::from(name) * NAME_COST + u32::from(apostrophe) * APOSTROPHE_COST
}

//one prefix or suffix entry: the flag a stem carries to take it, whether it combines with an
//affix of the other side, what it takes off the stem, what it puts on, and what the stem's end
//(or start, for a prefix) must look like
struct Affix {
    flag: char,
    cross_product: bool,
    strip: String,
    add: String,
    condition: Vec<CharClass>,
}

//one character position of an affix condition: `.`, `[abc]`, `[^abc]` or a plain character
enum CharClass {
    Any,
    OneOf(Vec<char>),
    NoneOf(Vec<char>),
}

//one piece of a compound rule: a flag, and whether it may repeat any number of times (`*`)
struct RulePart {
    flag: char,
    repeats: bool,
}

//how a word is capitalised, which decides the forms it may be found under
#[derive(Clone, Copy, PartialEq)]
enum Casing {
    Lower,
    Capitalised,
    AllCaps,
    Mixed,
}
