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
//! - `ICONV` (the typographic apostrophe read as `'`), `NOSUGGEST`, and for suggestions `TRY`,
//!   `REP`, `KEY`, `MAXNGRAMSUGS`, `MAXDIFF`, `ONLYMAXDIFF` and `NOSPLITSUGS`;
//! - `COMPOUNDRULE`, `COMPOUNDMIN` and `ONLYINCOMPOUND`, which these files use only for ordinals
//!   ("21st", "112th"), so compounding is only tried on a word that holds a digit;
//! - `WORDCHARS` is read past: the client splits text into words before it asks.
//!
//! Capitals follow Hunspell: a word the dictionary has in lower case is also right Capitalised
//! or in ALL CAPS, and one it has capitalised ("Paris") is right in ALL CAPS but not lower case.
//! A hyphenated word is right when each part is. Any other word holding a digit ("x86", "2024")
//! is not a word the dictionary can judge, so it passes.
//!
//! Suggestions follow Hunspell's suggester (`suggestmgr.cxx` in Hunspell 1.7), rebuilt here from
//! how it works rather than from its code: first the words one slip away, tried in Hunspell's
//! order (the word in capitals, common misspellings, letters swapped near or far, a neighbouring
//! key or a capital, an extra, missing or moved letter, a wrong one, a doubled pair, a missing
//! space), stopping there when the capitals or a common misspelling give a word; otherwise a few
//! of the dictionary words sharing the most letter runs with it follow (its n-gram suggester),
//! ranked by the same weights. Where Hunspell's choice among equal scores depends on its hash
//! table's layout, this one takes the .dic's order, so the tail of a list can differ.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

/// The most suggestions one misspelled word offers: 5, what Chromium's spellchecker shows
/// (`spellcheck::kMaxSuggestions`) and about what Firefox's menu lists; more is a list to read,
/// not a correction to pick.
pub const MAX_SUGGESTIONS: usize = 5;

//how many respellings are gathered before the edits stop: 15, Hunspell's MAXSUGGESTION. More
//are gathered than shown so the n-gram pass sees the same list it does in Hunspell
const SUGGESTION_POOL: usize = 15;

//the farthest apart two letters may be swapped, or one letter moved, as a slip: 4 places,
//Hunspell's MAX_CHAR_DISTANCE
const SLIP_DISTANCE_MAX: usize = 4;

//the keyboard a wrong key is looked for on when the .aff gives no KEY: Hunspell's default, the
//three letter rows of a QWERTY keyboard with `|` between the rows
const DEFAULT_KEYBOARD: &str = "qwertyuiop|asdfghjkl|zxcvbnm";

//the most n-gram respellings added after the slips when the .aff gives no MAXNGRAMSUGS: 4,
//Hunspell's default
const DEFAULT_NGRAM_SUGGESTIONS: usize = 4;

//the n-gram suggester keeps the 100 stems most like the word (Hunspell's MAX_ROOTS), grows at
//most 100 forms from each (MAX_WORDS) and keeps the 200 forms most like it (MAX_GUESS); these
//bound its work however long the word
const NGRAM_STEMS: usize = 100;
const NGRAM_FORMS_PER_STEM: usize = 100;
const NGRAM_GUESSES: usize = 200;

//a stem whose length differs from the word's by more than 4 letters is never compared, as
//Hunspell skips it to avoid strange suggestions; it also keeps a long nonsense word cheap
const NGRAM_LENGTH_SLACK: usize = 4;

//the longest letter run the first n-gram pass compares (3) and the final ranking's two passes
//(2 weighted, 4 plain), as Hunspell's ngsuggest uses them
const STEM_RUN_LONGEST: usize = 3;
const PAIR_RUN_LONGEST: usize = 2;
const RANK_RUN_LONGEST: usize = 4;

//Hunspell's weights in the final ranking: a guess that is the word in other capitals gets 2000;
//one over 1000 is excellent and shuts out the rest, one under -100 is poor; a guess sharing too
//few letter pairs with the word loses 1000; two letters swapped at a distance earn 10
const SAME_LETTERS_BONUS: i32 = 2000;
const EXCELLENT_SCORE: i32 = 1000;
const POOR_SCORE: i32 = -100;
const FEW_PAIRS_PENALTY: i32 = 1000;
const DISTANT_SWAP_BONUS: i32 = 10;

//MAXDIFF, when the .aff gives it, scales how many letter pairs a guess must share: the scale is
//(10 - MAXDIFF) / 5, so the default of none is a scale of 1
const MAXDIFF_BASE: f64 = 10.0;
const MAXDIFF_DIVISOR: f64 = 5.0;

//the keyboard string's row separator
const KEYBOARD_ROW_BREAK: char = '|';

/// A loaded dictionary: its stems with their flags, and the rules that grow and judge them.
pub struct Dictionary {
    //every .dic line as its stem and flags, in the file's order, which the n-gram suggester walks
    entries: Vec<(String, Vec<char>)>,
    //each stem with where its entries sit in `entries`; a stem listed twice keeps both
    stems: HashMap<String, Vec<usize>>,
    prefixes: Vec<Affix>,
    suffixes: Vec<Affix>,
    //what a word's characters are replaced by before it is looked up, such as ’ by '
    input_conversions: Vec<(String, String)>,
    //common misspellings as (wrong, right) pieces, tried first when suggesting
    replacements: Vec<(String, String)>,
    //the characters a suggestion may insert or swap in, most common first
    try_characters: Vec<char>,
    //the keyboard rows a wrong key is looked for on, `|` between rows
    keyboard: Vec<char>,
    //how many n-gram respellings may follow the slips; 0 turns that suggester off
    ngram_suggestions: usize,
    //MAXDIFF's scale on the letter pairs a guess must share, and ONLYMAXDIFF's "never a poor one"
    pair_scale: f64,
    only_max_diff: bool,
    //NOSPLITSUGS: never suggest splitting a word in two
    no_split: bool,
    nosuggest: Option<char>,
    only_in_compound: Option<char>,
    compound_rules: Vec<Vec<RulePart>>,
    compound_min: usize,
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

//how an n-gram score is docked for a difference in length: only when the other word is longer,
//or either way
#[derive(Clone, Copy)]
enum Lengths {
    LongerWorse,
    AnyMismatch,
}

//how a word is capitalised, which decides the forms it may be found under
#[derive(Clone, Copy, PartialEq)]
enum Casing {
    Lower,
    Capitalised,
    AllCaps,
    Mixed,
}

impl Dictionary {
    /// Read a dictionary from its two files' text.
    /// input: the `.aff` and the `.dic` as UTF-8 text
    /// output: the dictionary, or a message naming the first rule this checker does not implement
    pub fn parse(aff: &str, dic: &str) -> Result<Dictionary, String> {
        let mut dictionary = Dictionary {
            entries: Vec::new(),
            stems: HashMap::new(),
            prefixes: Vec::new(),
            suffixes: Vec::new(),
            input_conversions: Vec::new(),
            replacements: Vec::new(),
            try_characters: Vec::new(),
            keyboard: DEFAULT_KEYBOARD.chars().collect(),
            ngram_suggestions: DEFAULT_NGRAM_SUGGESTIONS,
            pair_scale: 1.0,
            only_max_diff: false,
            no_split: false,
            nosuggest: None,
            only_in_compound: None,
            compound_rules: Vec::new(),
            compound_min: 1,
        };
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
                ("WORDCHARS", _) => {}
                ("TRY", 2) => dictionary.try_characters = fields[1].chars().collect(),
                ("KEY", 2) => dictionary.keyboard = fields[1].chars().collect(),
                ("MAXNGRAMSUGS", 2) => {
                    dictionary.ngram_suggestions =
                        fields[1].parse().map_err(|_| line.to_string())?
                }
                ("MAXDIFF", 2) => {
                    let most: f64 = fields[1].parse().map_err(|_| line.to_string())?;
                    dictionary.pair_scale = (MAXDIFF_BASE - most) / MAXDIFF_DIVISOR;
                }
                ("ONLYMAXDIFF", 1) => dictionary.only_max_diff = true,
                ("NOSPLITSUGS", 1) => dictionary.no_split = true,
                ("NOSUGGEST", 2) => dictionary.nosuggest = flag(&fields),
                ("ONLYINCOMPOUND", 2) => dictionary.only_in_compound = flag(&fields),
                ("COMPOUNDMIN", 2) => {
                    dictionary.compound_min = fields[1].parse().map_err(|_| line.to_string())?
                }
                ("ICONV", 3) => dictionary
                    .input_conversions
                    .push((fields[1].to_string(), fields[2].to_string())),
                //an underscore in a replacement stands for a space ("alot" to "a lot")
                ("REP", 3) => dictionary
                    .replacements
                    .push((fields[1].to_string(), fields[2].replace('_', " "))),
                ("COMPOUNDRULE", 2) => dictionary.compound_rules.push(compound_rule(fields[1])?),
                ("PFX" | "SFX", 4) => {}
                ("PFX" | "SFX", 5) => {
                    let affix = affix(&fields, aff)?;
                    match directive {
                        "PFX" => dictionary.prefixes.push(affix),
                        _ => dictionary.suffixes.push(affix),
                    }
                }
                _ => return Err(format!("unsupported .aff rule: {line}")),
            }
        }

        //the first line of a .dic is its count; every other is a stem and its flags
        for line in dic.lines().skip(1) {
            let entry = line.split('\t').next().unwrap_or("").trim();
            if entry.is_empty() {
                continue;
            }
            let (stem, flags) = entry.split_once('/').unwrap_or((entry, ""));
            dictionary
                .stems
                .entry(stem.to_string())
                .or_default()
                .push(dictionary.entries.len());
            dictionary
                .entries
                .push((stem.to_string(), flags.chars().collect()));
        }
        Ok(dictionary)
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

    //the respellings of one word with no hyphen, as Hunspell orders them: the slips first, tried
    //on the word as typed and then in the capitals the dictionary may list it under, then the
    //n-gram guesses unless the slips found the answer, all put back into the word's capitals
    fn respell(&self, word: &str, personal: &HashSet<String>) -> Vec<String> {
        let casing = casing(word);
        let as_typed: Vec<char> = word.chars().collect();
        let lower: Vec<char> = word.to_lowercase().chars().collect();
        let mut found: Vec<String> = Vec::new();
        //only the last pass decides whether the slips found the answer, as in Hunspell
        let answered = match casing {
            Casing::Lower | Casing::Mixed => self.slips(&as_typed, personal, &mut found),
            Casing::Capitalised => {
                self.slips(&as_typed, personal, &mut found);
                self.slips(&lower, personal, &mut found)
            }
            Casing::AllCaps => {
                self.slips(&lower, personal, &mut found);
                let capitalised: Vec<char> = recase(&word.to_lowercase(), Casing::Capitalised)
                    .chars()
                    .collect();
                self.slips(&capitalised, personal, &mut found)
            }
        };
        if !answered && self.ngram_suggestions > 0 {
            //a lower-case word is not offered names, which the dictionary has capitalised
            match casing {
                Casing::Lower => self.ngram_guesses(&as_typed, true, personal, &mut found),
                _ => self.ngram_guesses(&lower, false, personal, &mut found),
            }
        }
        match casing {
            Casing::Capitalised | Casing::AllCaps => found
                .iter()
                .map(|respelling| recase(respelling, casing))
                .collect(),
            Casing::Lower | Casing::Mixed => found,
        }
    }

    //add the words one slip away from a word to `found`, in the order Hunspell tries the slips
    //and each in the order it walks the word
    //input: the word as characters, the person's words, the respellings so far
    //output: whether the word in capitals or a listed common misspelling (REP) is a word, which
    //  Hunspell takes as the answer: the other slips and the n-gram guesses are then skipped
    fn slips(&self, word: &[char], personal: &HashSet<String>, found: &mut Vec<String>) -> bool {
        let length = word.len();
        let text: String = word.iter().collect();
        let mut candidate: Vec<char> = word.to_vec();

        //the word in capitals, for an abbreviation typed in lower case ("html")
        let before_capitals = found.len();
        self.offer(&text.to_uppercase(), personal, found);
        let mut answered = found.len() > before_capitals;

        //the listed common misspellings, at every place each one occurs, overlaps included
        for (wrong, right) in &self.replacements {
            let mut from = 0;
            while let Some(offset) = text[from..].find(wrong.as_str()) {
                let at = from + offset;
                let replaced = format!("{}{right}{}", &text[..at], &text[at + wrong.len()..]);
                let before = found.len();
                self.offer(&replaced, personal, found);
                answered |= found.len() > before;
                //a replacement holding a space ("alot" to "a lot") is good when the words
                //before each space are words and what follows is one
                let mut previous = 0;
                for (space, _) in replaced.match_indices(' ') {
                    if self.suggestible(&replaced[previous..space], personal) {
                        let before = found.len();
                        self.offer(&replaced[space + 1..], personal, found);
                        if found.len() > before {
                            found.pop();
                            found.push(replaced.clone());
                        }
                    }
                    previous = space + 1;
                }
                from = at + wrong.chars().next().map_or(1, char::len_utf8);
            }
        }
        //the word in capitals, or a listed misspelling that is a word as it stands, is taken as
        //the answer, and Hunspell tries nothing else; a listed one split by a space ("a lot") is
        //not
        if answered {
            return true;
        }

        //two neighbouring letters swapped, and in a word of four or five letters two pairs of
        //them ("ahev" to "have", "owudl" to "would")
        for at in 0..length.saturating_sub(1) {
            candidate.swap(at, at + 1);
            self.offer_characters(&candidate, personal, found);
            candidate.swap(at, at + 1);
        }
        if length == 4 || length == 5 {
            candidate[0] = word[1];
            candidate[1] = word[0];
            candidate[2] = word[2];
            candidate[length - 2] = word[length - 1];
            candidate[length - 1] = word[length - 2];
            self.offer_characters(&candidate, personal, found);
            if length == 5 {
                candidate[0] = word[0];
                candidate[1] = word[2];
                candidate[2] = word[1];
                self.offer_characters(&candidate, personal, found);
            }
            candidate.copy_from_slice(word);
        }

        //two letters a little further apart swapped
        for first in 0..length {
            for second in 0..length {
                let distance = first.abs_diff(second);
                if distance > 1 && distance <= SLIP_DISTANCE_MAX {
                    candidate.swap(first, second);
                    self.offer_characters(&candidate, personal, found);
                    candidate.swap(first, second);
                }
            }
        }

        //a letter that should be a capital, or a key next to the right one on the keyboard
        for at in 0..length {
            let original = word[at];
            let mut upper = original.to_uppercase();
            if let (Some(capital), None) = (upper.next(), upper.next())
                && capital != original
            {
                candidate[at] = capital;
                self.offer_characters(&candidate, personal, found);
            }
            for (place, _) in self
                .keyboard
                .iter()
                .enumerate()
                .filter(|(_, key)| **key == original)
            {
                let left = place.checked_sub(1).map(|left| self.keyboard[left]);
                let right = self.keyboard.get(place + 1).copied();
                for neighbour in [left, right].into_iter().flatten() {
                    if neighbour != KEYBOARD_ROW_BREAK {
                        candidate[at] = neighbour;
                        self.offer_characters(&candidate, personal, found);
                    }
                }
            }
            candidate[at] = original;
        }

        //an extra letter, taken out from the end of the word backwards
        if length >= 2 {
            for at in (0..length).rev() {
                let removed = candidate.remove(at);
                self.offer_characters(&candidate, personal, found);
                candidate.insert(at, removed);
            }
        }

        //a missing letter: each TRY character put in at every place, from the end backwards
        for &character in &self.try_characters {
            for at in (0..=length).rev() {
                candidate.insert(at, character);
                self.offer_characters(&candidate, personal, found);
                candidate.remove(at);
            }
        }

        //a letter typed two or three places early, moved forward; then one typed late, moved
        //back (a move of one place is a swap, tried above)
        for from in 0..length {
            for to in from + 1..length.min(from + SLIP_DISTANCE_MAX) {
                candidate.swap(to, to - 1);
                if to - from >= 2 {
                    self.offer_characters(&candidate, personal, found);
                }
            }
            candidate.copy_from_slice(word);
        }
        for from in (1..length).rev() {
            for to in (from.saturating_sub(SLIP_DISTANCE_MAX - 1)..from).rev() {
                candidate.swap(to, to + 1);
                if from - to >= 2 {
                    self.offer_characters(&candidate, personal, found);
                }
            }
            candidate.copy_from_slice(word);
        }

        //a wrong letter: each TRY character in place of each letter, from the end backwards
        for &character in &self.try_characters {
            for at in (0..length).rev() {
                if word[at] == character {
                    continue;
                }
                candidate[at] = character;
                self.offer_characters(&candidate, personal, found);
                candidate[at] = word[at];
            }
        }

        //a pair of letters typed twice ("vacacation" to "vacation"): the same letter two
        //places back, three times in a row or twice from the fifth letter on
        if length >= 5 {
            let mut repeats = 0;
            for at in 2..length {
                if word[at] != word[at - 2] {
                    repeats = 0;
                    continue;
                }
                repeats += 1;
                if repeats == 3 || (repeats == 2 && at >= 4) {
                    let shorter: Vec<char> = word[..at - 1]
                        .iter()
                        .chain(&word[at + 1..])
                        .copied()
                        .collect();
                    self.offer_characters(&shorter, personal, found);
                    repeats = 0;
                }
            }
        }

        //a missing space, at every place where both halves are words; with a hyphen too when
        //neither half is a single letter and the language joins words that way (its TRY has
        //`a` or `-`, as Hunspell judges it)
        let joins_with_hyphen =
            self.try_characters.contains(&'a') || self.try_characters.contains(&'-');
        if !self.no_split && length >= 3 {
            for at in 1..length {
                let first: String = word[..at].iter().collect();
                let second: String = word[at..].iter().collect();
                if !self.suggestible(&second, personal) || !self.suggestible(&first, personal) {
                    continue;
                }
                let mut pairs = vec![format!("{first} {second}")];
                if joins_with_hyphen && at > 1 && length - at > 1 {
                    pairs.push(format!("{first}-{second}"));
                }
                for pair in pairs {
                    if found.len() < SUGGESTION_POOL && !found.contains(&pair) {
                        found.push(pair);
                    }
                }
            }
        }
        answered
    }

    //add to `found` the dictionary words most like a word by their shared letter runs, as
    //Hunspell's n-gram suggester picks them
    //input: the word as characters (lower case unless it mixes capitals), whether names are kept
    //  out, the person's words, the respellings so far
    //output: up to the .aff's MAXNGRAMSUGS more respellings in `found`
    fn ngram_guesses(
        &self,
        word: &[char],
        lower_case_only: bool,
        personal: &HashSet<String>,
        found: &mut Vec<String>,
    ) {
        let length = word.len();
        //every stem, then the person's words as stems with no flags
        let no_flags: Vec<char> = Vec::new();
        let stems = self
            .entries
            .iter()
            .map(|(stem, flags)| (stem.as_str(), flags))
            .chain(personal.iter().map(|word| (word.as_str(), &no_flags)));

        //first, the stems most like the word, compared in lower case; a slot holds the stem and
        //its score, the empty slots starting below anything real
        let mut best_stems: Vec<(i32, Option<(&str, &Vec<char>)>)> = (0..NGRAM_STEMS)
            .map(|slot| (slot_floor(slot), None))
            .collect();
        let mut lowest = NGRAM_STEMS - 1;
        //the flags that keep a stem out, and two buffers reused across the stems, since this
        //loop runs over the whole dictionary
        let never: Vec<char> = [self.nosuggest, self.only_in_compound]
            .into_iter()
            .flatten()
            .collect();
        let mut stem_characters: Vec<char> = Vec::new();
        let mut lowered: Vec<char> = Vec::new();
        for (stem, flags) in stems {
            if never.iter().any(|flag| flags.contains(flag)) {
                continue;
            }
            stem_characters.clear();
            stem_characters.extend(stem.chars());
            if stem_characters.len().abs_diff(length) > NGRAM_LENGTH_SLACK {
                continue;
            }
            let capitalised = stem_characters
                .iter()
                .any(|character| character.is_uppercase());
            if lower_case_only
                && stem_characters
                    .first()
                    .is_some_and(|first| first.is_uppercase())
            {
                continue;
            }
            //most stems are lower case already and are compared as they are
            let compared: &[char] = match capitalised {
                true => {
                    lowered.clear();
                    lowered.extend(stem_characters.iter().flat_map(|c| c.to_lowercase()));
                    &lowered
                }
                false => &stem_characters,
            };
            let score = ngram(
                STEM_RUN_LONGEST,
                word,
                compared,
                Lengths::LongerWorse,
                false,
            ) + left_common(word, &stem_characters);
            keep_if_better(&mut best_stems, &mut lowest, score, (stem, flags));
        }

        //a guess must beat the word scored against three copies of itself with every fourth
        //letter blotted out, starting at its second, third and fourth letters
        let mut threshold = 0;
        for start in 1..4 {
            let mut blotted = word.to_vec();
            for at in (start..length).step_by(4) {
                blotted[at] = '*';
            }
            threshold += ngram(length, word, &blotted, Lengths::AnyMismatch, false);
        }
        let threshold = threshold / 3 - 1;

        //then every form the kept stems grow into that looks like the word's ends, scored on
        //runs of every length
        let mut guesses: Vec<(i32, Option<String>)> = (0..NGRAM_GUESSES)
            .map(|slot| (slot_floor(slot), None))
            .collect();
        let mut lowest = NGRAM_GUESSES - 1;
        for (_, stem) in &best_stems {
            let Some((stem, flags)) = stem else {
                continue;
            };
            for form in self.grow(stem, flags, word) {
                let form_characters: Vec<char> = form.chars().collect();
                let lowered: Vec<char> = form.to_lowercase().chars().collect();
                let score = ngram(length, word, &lowered, Lengths::AnyMismatch, false)
                    + left_common(word, &form_characters);
                if score > threshold {
                    keep_if_better(&mut guesses, &mut lowest, score, form);
                }
            }
        }
        let mut guesses: Vec<(i32, String)> = guesses
            .into_iter()
            .filter_map(|(score, guess)| guess.map(|guess| (score, guess)))
            .collect();
        guesses.sort_by_key(|(score, _)| Reverse(*score));

        //rank them again on how much of the word they keep in order, its start, its letters in
        //place, a far swap, runs of up to four and weighted pairs both ways; a guess that is the
        //word in other capitals ends the ranking there, as Hunspell's does
        let lowered_word: Vec<char> = word
            .iter()
            .collect::<String>()
            .to_lowercase()
            .chars()
            .collect();
        for (score, guess) in guesses.iter_mut() {
            let guess_lower: Vec<char> = guess.to_lowercase().chars().collect();
            let guess_length = guess_lower.len();
            let common = longest_common_subsequence(word, &guess_lower);
            if length == guess_length && length == common {
                *score += SAME_LETTERS_BONUS;
                break;
            }
            let pairs = ngram(
                PAIR_RUN_LONGEST,
                word,
                &guess_lower,
                Lengths::AnyMismatch,
                true,
            ) + ngram(
                PAIR_RUN_LONGEST,
                &guess_lower,
                &lowered_word,
                Lengths::AnyMismatch,
                true,
            );
            let (in_place, swapped) = letters_in_place(word, &guess_lower);
            let too_few_pairs = (pairs as f64) < (length + guess_length) as f64 * self.pair_scale;
            *score = 2 * common as i32 - length.abs_diff(guess_length) as i32
                + left_common(word, &guess_lower)
                + i32::from(in_place > 0)
                + if swapped { DISTANT_SWAP_BONUS } else { 0 }
                + ngram(
                    RANK_RUN_LONGEST,
                    word,
                    &guess_lower,
                    Lengths::AnyMismatch,
                    false,
                )
                + pairs
                - if too_few_pairs { FEW_PAIRS_PENALTY } else { 0 };
        }
        guesses.sort_by_key(|(score, _)| Reverse(*score));

        //take the best few: only excellent ones once one is excellent, and a poor one only when
        //nothing else came, never one holding an earlier respelling ("naively" keeps out
        //"naivelyness")
        let before = found.len();
        let mut closed = false;
        for (score, guess) in guesses {
            if found.len() >= before + self.ngram_suggestions || found.len() >= SUGGESTION_POOL {
                break;
            }
            if closed && score <= EXCELLENT_SCORE {
                continue;
            }
            if score > EXCELLENT_SCORE {
                closed = true;
            } else if score < POOR_SCORE {
                closed = true;
                if found.len() > before || self.only_max_diff {
                    continue;
                }
            }
            let repeats = found.iter().any(|earlier| guess.contains(earlier.as_str()));
            if !repeats && self.suggestible(&guess, personal) {
                found.push(guess);
            }
        }
    }

    //the forms a stem grows into that could be the word: the stem itself, then each suffix its
    //flags allow whose ending the word has, a prefix on those that combine, and each prefix
    //alone whose beginning the word has; at most `NGRAM_FORMS_PER_STEM`
    fn grow(&self, stem: &str, flags: &[char], word: &[char]) -> Vec<String> {
        let word: String = word.iter().collect();
        let word_length = word.len();
        //an affix is only worth growing when the word carries what it adds
        let ends_like = |affix: &Affix| {
            affix.add.is_empty()
                || (word_length > affix.add.len() && word.ends_with(affix.add.as_str()))
        };
        let starts_like = |affix: &Affix| {
            affix.add.is_empty()
                || (word_length > affix.add.len() && word.starts_with(affix.add.as_str()))
        };
        //Hunspell walks one flag's affixes last-listed first
        let with_flag = |affixes: &'_ [Affix], flag: char| -> Vec<usize> {
            (0..affixes.len())
                .rev()
                .filter(|&at| affixes[at].flag == flag)
                .collect()
        };

        let mut forms: Vec<(String, bool)> = Vec::new();
        if !self
            .only_in_compound
            .is_some_and(|flag| flags.contains(&flag))
        {
            forms.push((stem.to_string(), false));
        }
        for &flag in flags {
            for at in with_flag(&self.suffixes, flag) {
                let suffix = &self.suffixes[at];
                if let Some(form) = ends_like(suffix)
                    .then(|| suffix.apply_suffix(stem))
                    .flatten()
                {
                    forms.push((form, suffix.cross_product));
                }
            }
        }
        //a prefix on each suffixed form that combines (Hunspell starts after the first form)
        let suffixed = forms.len();
        for index in 1..suffixed {
            if !forms[index].1 {
                continue;
            }
            for &flag in flags {
                for at in with_flag(&self.prefixes, flag) {
                    let prefix = &self.prefixes[at];
                    if !prefix.cross_product || !starts_like(prefix) {
                        continue;
                    }
                    if let Some(form) = prefix.apply_prefix(&forms[index].0) {
                        forms.push((form, true));
                    }
                }
            }
        }
        for &flag in flags {
            for at in with_flag(&self.prefixes, flag) {
                let prefix = &self.prefixes[at];
                if let Some(form) = starts_like(prefix)
                    .then(|| prefix.apply_prefix(stem))
                    .flatten()
                {
                    forms.push((form, prefix.cross_product));
                }
            }
        }
        forms
            .into_iter()
            .take(NGRAM_FORMS_PER_STEM)
            .map(|(form, _)| form)
            .collect()
    }

    //add a respelling to `found` when it is a word that may be suggested, not there already, and
    //the pool has room
    fn offer(&self, candidate: &str, personal: &HashSet<String>, found: &mut Vec<String>) {
        if found.len() < SUGGESTION_POOL
            && !found.iter().any(|earlier| earlier == candidate)
            && self.suggestible(candidate, personal)
        {
            found.push(candidate.to_string());
        }
    }

    fn offer_characters(
        &self,
        candidate: &[char],
        personal: &HashSet<String>,
        found: &mut Vec<String>,
    ) {
        self.offer(&candidate.iter().collect::<String>(), personal, found);
    }

    //whether a respelling may be suggested exactly as written: the person's word, or a stem or
    //affixed form not marked never-to-suggest, or an ordinal; unlike checking, no other capitals
    //count, as Hunspell's suggester looks each candidate up as it is
    fn suggestible(&self, candidate: &str, personal: &HashSet<String>) -> bool {
        if candidate.is_empty() {
            return false;
        }
        if candidate
            .chars()
            .any(|character| character.is_ascii_digit())
        {
            return self.compound(candidate);
        }
        personal.contains(candidate) || self.lookup(candidate, false)
    }

    //a word with its input conversions applied, such as ’ read as '
    fn convert(&self, word: &str) -> String {
        let mut converted = word.to_string();
        for (from, to) in &self.input_conversions {
            converted = converted.replace(from.as_str(), to);
        }
        converted
    }

    //check a converted word; `nosuggest` says whether stems marked never-to-suggest count, which
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
        forms
            .iter()
            .any(|form| personal.contains(form) || self.lookup(form, nosuggest))
    }

    //whether one exact form is a stem, or a stem with a suffix, a prefix or both
    fn lookup(&self, word: &str, nosuggest: bool) -> bool {
        //a stem with no affix
        if self.has_stem(word, &[], nosuggest) {
            return true;
        }
        for suffix in &self.suffixes {
            let Some(stem) = suffix.unapply_suffix(word) else {
                continue;
            };
            if self.has_stem(&stem, &[suffix.flag], nosuggest) {
                return true;
            }
            //a prefix on top of the suffix, when both combine
            if !suffix.cross_product {
                continue;
            }
            for prefix in self.prefixes.iter().filter(|prefix| prefix.cross_product) {
                if let Some(root) = prefix.unapply_prefix(&stem)
                    && self.has_stem(&root, &[prefix.flag, suffix.flag], nosuggest)
                {
                    return true;
                }
            }
        }
        self.prefixes.iter().any(|prefix| {
            prefix
                .unapply_prefix(word)
                .is_some_and(|stem| self.has_stem(&stem, &[prefix.flag], nosuggest))
        })
    }

    //whether a stem is listed carrying every flag asked for, not only inside compounds, and not
    //never-to-suggest unless those count
    fn has_stem(&self, stem: &str, flags: &[char], nosuggest: bool) -> bool {
        let Some(listed) = self.stems.get(stem) else {
            return false;
        };
        listed.iter().map(|&at| &self.entries[at].1).any(|entry| {
            flags.iter().all(|flag| entry.contains(flag))
                && !self
                    .only_in_compound
                    .is_some_and(|flag| entry.contains(&flag))
                && (nosuggest || !self.nosuggest.is_some_and(|flag| entry.contains(&flag)))
        })
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
            let listed = self.stems.get(&piece).is_some_and(|listed| {
                listed
                    .iter()
                    .any(|&at| self.entries[at].1.contains(&part.flag))
            });
            let next = if part.repeats { rule } else { rest };
            listed && self.compound_from(&characters[length..], next, taken + 1)
        })
    }
}

impl Affix {
    //the stem a word came from if this suffix made it: the added end taken off, the stripped
    //end put back, and the condition holding at the stem's end
    fn unapply_suffix(&self, word: &str) -> Option<String> {
        let base = word.strip_suffix(self.add.as_str())?;
        if base.is_empty() && self.strip.is_empty() {
            return None;
        }
        let stem = format!("{base}{}", self.strip);
        self.end_fits(&stem).then_some(stem)
    }

    //the stem a word came from if this prefix made it, the condition holding at its start
    fn unapply_prefix(&self, word: &str) -> Option<String> {
        let base = word.strip_prefix(self.add.as_str())?;
        if base.is_empty() && self.strip.is_empty() {
            return None;
        }
        let stem = format!("{}{base}", self.strip);
        self.start_fits(&stem).then_some(stem)
    }

    //the word this suffix makes of a stem: the stripped end taken off and the added end put on,
    //when the stem is longer than what is stripped and its end meets the condition
    fn apply_suffix(&self, stem: &str) -> Option<String> {
        let base = stem.strip_suffix(self.strip.as_str())?;
        (!base.is_empty() && self.end_fits(stem)).then(|| format!("{base}{}", self.add))
    }

    //the word this prefix makes of a stem, its condition holding at the stem's start
    fn apply_prefix(&self, stem: &str) -> Option<String> {
        let base = stem.strip_prefix(self.strip.as_str())?;
        (!base.is_empty() && self.start_fits(stem)).then(|| format!("{}{base}", self.add))
    }

    //whether a stem's last letters meet a suffix's condition, one class per letter
    fn end_fits(&self, stem: &str) -> bool {
        let tail: Vec<char> = stem.chars().rev().take(self.condition.len()).collect();
        tail.len() == self.condition.len()
            && tail
                .iter()
                .zip(self.condition.iter().rev())
                .all(|(&character, class)| class.matches(character))
    }

    //whether a stem's first letters meet a prefix's condition
    fn start_fits(&self, stem: &str) -> bool {
        let head: Vec<char> = stem.chars().take(self.condition.len()).collect();
        head.len() == self.condition.len()
            && head
                .iter()
                .zip(&self.condition)
                .all(|(&character, class)| class.matches(character))
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

//how many of one word's letter runs, of each length up to `longest`, turn up anywhere in another,
//as Hunspell's n-gram suggester counts them, less the difference in length past two
//input: the run length to stop at, the word, the word compared with, how length differences
//  count, and whether a run that is missing costs a point (two at either end of the word)
//output: the score; unweighted, counting stops at the first length with fewer than two runs found
fn ngram(longest: usize, word: &[char], other: &[char], lengths: Lengths, weighted: bool) -> i32 {
    if other.is_empty() {
        return 0;
    }
    let mut score = 0;
    for run_length in 1..=longest {
        let mut runs_found: i32 = 0;
        for at in 0..(word.len() + 1).saturating_sub(run_length) {
            let run = &word[at..at + run_length];
            if contains_run(other, run) {
                runs_found += 1;
            } else if weighted {
                runs_found -= 1;
                if at == 0 || at == word.len() - run_length {
                    runs_found -= 1;
                }
            }
        }
        score += runs_found;
        if runs_found < 2 && !weighted {
            break;
        }
    }
    let longer_by = other.len() as i32 - word.len() as i32;
    let docked = match lengths {
        Lengths::LongerWorse => longer_by - 2,
        Lengths::AnyMismatch => longer_by.abs() - 2,
    };
    score - docked.max(0)
}

//whether a run of letters turns up anywhere in a word; plain loops, since this runs for every
//stem in the dictionary on each suggestion
fn contains_run(word: &[char], run: &[char]) -> bool {
    let mut start = 0;
    while start + run.len() <= word.len() {
        let mut matched = 0;
        while matched < run.len() && word[start + matched] == run[matched] {
            matched += 1;
        }
        if matched == run.len() {
            return true;
        }
        start += 1;
    }
    false
}

//how many letters two words share from the start, the other's first letter allowed to be a
//capital ("Paris" for "parsi"); 0 when even the first letters differ
fn left_common(word: &[char], other: &[char]) -> i32 {
    let (Some(&first), Some(&other_first)) = (word.first(), other.first()) else {
        return 0;
    };
    let other_first_lower = other_first.to_lowercase().next().unwrap_or(other_first);
    if first != other_first && first != other_first_lower {
        return 0;
    }
    let shared = word
        .iter()
        .zip(other)
        .skip(1)
        .take_while(|(letter, other_letter)| letter == other_letter)
        .count();
    1 + shared as i32
}

//how many places two words have the same letter, and whether they are the same length and
//differ in just two places holding each other's letters (a swap at a distance)
fn letters_in_place(word: &[char], guess: &[char]) -> (usize, bool) {
    let mut same = 0;
    let mut differing: Vec<usize> = Vec::new();
    for (at, (letter, guess_letter)) in word.iter().zip(guess).enumerate() {
        if letter == guess_letter {
            same += 1;
        } else {
            differing.push(at);
        }
    }
    let swapped = word.len() == guess.len()
        && differing.len() == 2
        && word[differing[0]] == guess[differing[1]]
        && word[differing[1]] == guess[differing[0]];
    (same, swapped)
}

//the length of the longest run of letters two words have in the same order, gaps allowed
fn longest_common_subsequence(word: &[char], other: &[char]) -> usize {
    let mut previous_row = vec![0; other.len() + 1];
    for &letter in word {
        let mut row = vec![0; other.len() + 1];
        for (at, &other_letter) in other.iter().enumerate() {
            row[at + 1] = match letter == other_letter {
                true => previous_row[at] + 1,
                false => row[at].max(previous_row[at + 1]),
            };
        }
        previous_row = row;
    }
    previous_row[other.len()]
}

//where an empty slot of a best-of list starts: each lower than the one before, as Hunspell
//seeds them, so the first real scores fill the slots from the end
fn slot_floor(slot: usize) -> i32 {
    -100 * slot as i32
}

//put an item in a best-of list when it beats the lowest slot, then find the new lowest; ties
//keep the earlier item, as Hunspell's lists do
fn keep_if_better<T>(slots: &mut [(i32, Option<T>)], lowest: &mut usize, score: i32, item: T) {
    if score <= slots[*lowest].0 {
        return;
    }
    slots[*lowest] = (score, Some(item));
    let mut lowest_score = score;
    for (at, (slot_score, _)) in slots.iter().enumerate() {
        if *slot_score < lowest_score {
            *lowest = at;
            lowest_score = *slot_score;
        }
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
