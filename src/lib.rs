//! The spelling checker: it reads a Hunspell dictionary (an `.aff` of rules and a `.dic` of
//! stems with their rule flags) and says whether a word is spelled right and, if not, what the
//! person probably meant.
//!
//! It implements exactly what the shipped English dictionaries (SCOWL's, as the npm packages
//! `dictionary-en` and `dictionary-en-gb` publish them; the `dictionaries` folder of this
//! repository) use, and refuses an `.aff` that asks for more rather than quietly checking it
//! wrong:
//! - UTF-8 text (`SET UTF-8`) with single-character flags (no `FLAG` line);
//! - `PFX`/`SFX` with strip, add and condition, a prefix and a suffix combining when both allow
//!   it (cross product), no affix carrying flags of its own;
//! - `ICONV` (the typographic apostrophe read as `'`), `NOSUGGEST`, `TRY` and `REP` for
//!   suggestions;
//! - `COMPOUNDRULE`, `COMPOUNDMIN` and `ONLYINCOMPOUND`, which these files use only for ordinals
//!   ("21st", "112th"), so compounding is only tried on a word that holds a digit;
//! - `WORDCHARS` is read past: the caller splits text into words before it asks.
//!
//! Capitals follow Hunspell: a word the dictionary has in lower case is also right Capitalised
//! or in ALL CAPS, and one it has capitalised ("Paris") is right in ALL CAPS but not lower case.
//! A hyphenated word is right when each part is. Any other word holding a digit ("x86", "2024")
//! is not a word the dictionary can judge, so it passes.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

/// The most suggestions one misspelled word offers: 5, what Chromium's spellchecker shows
/// (`spellcheck::kMaxSuggestions`) and about what Firefox's menu lists; more is a list to read,
/// not a correction to pick.
pub const MAX_SUGGESTIONS: usize = 5;

//the shortest half a missing space may split a word into: 2, since every single letter is a word
//in the shipped dictionaries and "t eh" is no respelling of "teh"
const SPLIT_PART_MIN: usize = 2;

//the longest letter run two words are compared on when ranking suggestions: 3, as Hunspell's
//n-gram suggester compares runs of one to three letters
const NGRAM_LONGEST: usize = 3;

/// A loaded dictionary: its stems with their flags, and the rules that grow and judge them.
pub struct Dictionary {
    //each stem with the flag sets of its entries; a stem listed twice keeps both sets
    stems: HashMap<String, Vec<Vec<char>>>,
    prefixes: Vec<Affix>,
    suffixes: Vec<Affix>,
    //what a word's characters are replaced by before it is looked up, such as ’ by '
    input_conversions: Vec<(String, String)>,
    //common misspellings as (wrong, right) pieces, tried first when suggesting
    replacements: Vec<(String, String)>,
    //the characters a suggestion may insert or swap in, most common first
    try_characters: Vec<char>,
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

//the kinds of one-character slip, in the order Hunspell tries them
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Slip {
    Swapped,
    Extra,
    Missing,
    Wrong,
    Split,
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
            stems: HashMap::new(),
            prefixes: Vec::new(),
            suffixes: Vec::new(),
            input_conversions: Vec::new(),
            replacements: Vec::new(),
            try_characters: Vec::new(),
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
                .push(flags.chars().collect());
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
        let casing = casing(&converted);
        //edits are made in lower case and put back into the word's capitals afterwards
        let working: Vec<char> = match casing {
            Casing::Capitalised | Casing::AllCaps => converted.to_lowercase().chars().collect(),
            _ => converted.chars().collect(),
        };
        let mut listed: Vec<String> = Vec::new();

        //the common misspellings first, at every place each one occurs
        let text: String = working.iter().collect();
        for (wrong, right) in &self.replacements {
            for (at, _) in text.match_indices(wrong.as_str()) {
                listed.push(format!(
                    "{}{right}{}",
                    &text[..at],
                    &text[at + wrong.len()..]
                ));
            }
        }
        //then the one-character slips, in the order Hunspell tries them: two letters swapped, an
        //extra letter, a missing one, a wrong one, and last a missing space
        let lower_case_try: Vec<char> = self
            .try_characters
            .iter()
            .copied()
            .filter(|character| !character.is_uppercase())
            .collect();
        let spelled = |characters: &[char]| characters.iter().collect::<String>();
        let mut slips: Vec<(Slip, String)> = Vec::new();
        for at in 0..working.len().saturating_sub(1) {
            let mut swapped = working.clone();
            swapped.swap(at, at + 1);
            slips.push((Slip::Swapped, spelled(&swapped)));
        }
        for at in 0..working.len() {
            let mut shorter = working.clone();
            shorter.remove(at);
            slips.push((Slip::Extra, spelled(&shorter)));
        }
        for at in 0..=working.len() {
            for &character in &lower_case_try {
                let mut longer = working.clone();
                longer.insert(at, character);
                slips.push((Slip::Missing, spelled(&longer)));
            }
        }
        for at in 0..working.len() {
            for &character in lower_case_try.iter().filter(|&&c| c != working[at]) {
                let mut replaced = working.clone();
                replaced[at] = character;
                slips.push((Slip::Wrong, spelled(&replaced)));
            }
        }
        //every single letter is a word in these dictionaries, so a split keeps two real words
        for at in SPLIT_PART_MIN..=working.len().saturating_sub(SPLIT_PART_MIN) {
            let halves = (spelled(&working[..at]), spelled(&working[at..]));
            slips.push((Slip::Split, format!("{} {}", halves.0, halves.1)));
        }

        //keep the candidates that are words, in the word's own capitals; a lower-case slip may
        //also be a name ("londn" for "London")
        let as_word = |candidate: &String| {
            let forms = [
                recase(candidate, casing),
                recase(candidate, Casing::Capitalised),
            ];
            let tried = if casing == Casing::Lower { 2 } else { 1 };
            forms.into_iter().take(tried).find(|form| {
                form.split(' ')
                    .all(|part| self.check_converted(part, personal, false))
            })
        };
        let replaced: Vec<String> = listed.iter().filter_map(as_word).collect();
        let mut slipped: Vec<(Slip, String)> = slips
            .iter()
            .filter_map(|(tier, slip)| as_word(slip).map(|form| (*tier, form)))
            .collect();
        //a typo rarely falls on the first letter, so slips that keep it go first; then two
        //letters swapped, the commonest typo, which Hunspell offers before anything else; a
        //split into two words last; between those the closest to the word, and between equals
        //the earlier kind of slip. The sort is stable, so
        //Hunspell's order holds after that
        let first = working.first().copied();
        slipped.sort_by_key(|(tier, slip)| {
            let lower: Vec<char> = slip.to_lowercase().chars().collect();
            (
                lower.first().copied() != first,
                *tier != Slip::Swapped,
                *tier == Slip::Split,
                Reverse(closeness(&working, &lower)),
                *tier,
            )
        });
        let slipped = slipped.into_iter().map(|(_, form)| form);

        //the listed misspellings first, then the slips, each once and never the word itself
        let mut found: Vec<String> = Vec::new();
        for form in replaced.into_iter().chain(slipped) {
            let form = match typographic {
                true => form.replace('\'', "’"),
                false => form,
            };
            if form != word && !found.contains(&form) {
                found.push(form);
            }
            if found.len() == MAX_SUGGESTIONS {
                break;
            }
        }
        found
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
        let Some(entries) = self.stems.get(stem) else {
            return false;
        };
        entries.iter().any(|entry| {
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
            let listed = self
                .stems
                .get(&piece)
                .is_some_and(|entries| entries.iter().any(|entry| entry.contains(&part.flag)));
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
        let tail: Vec<char> = stem.chars().rev().take(self.condition.len()).collect();
        let holds = tail.len() == self.condition.len()
            && tail
                .iter()
                .zip(self.condition.iter().rev())
                .all(|(&character, class)| class.matches(character));
        holds.then_some(stem)
    }

    //the stem a word came from if this prefix made it, the condition holding at its start
    fn unapply_prefix(&self, word: &str) -> Option<String> {
        let base = word.strip_prefix(self.add.as_str())?;
        if base.is_empty() && self.strip.is_empty() {
            return None;
        }
        let stem = format!("{}{base}", self.strip);
        let head: Vec<char> = stem.chars().take(self.condition.len()).collect();
        let holds = head.len() == self.condition.len()
            && head
                .iter()
                .zip(&self.condition)
                .all(|(&character, class)| class.matches(character));
        holds.then_some(stem)
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

//how close one word is to another: the letters, letter pairs and letter triples of one found in
//the other, each found once, less the difference in their lengths, which is how Hunspell's n-gram
//suggester scores closeness
fn closeness(word: &[char], other: &[char]) -> isize {
    let shared: usize = (1..=NGRAM_LONGEST)
        .map(|length| {
            let mut unmatched: Vec<&[char]> = other.windows(length).collect();
            word.windows(length)
                .filter(|run| {
                    let at = unmatched.iter().position(|candidate| candidate == run);
                    at.map(|at| unmatched.swap_remove(at)).is_some()
                })
                .count()
        })
        .sum();
    shared as isize - word.len().abs_diff(other.len()) as isize
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
