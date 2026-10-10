# cowrite-spell

A small Hunspell-format spellchecker in pure Rust, with no dependencies. It implements exactly the
affix rules the English SCOWL dictionaries use (the `dictionary-en` and `dictionary-en-gb` npm
packages, copied here under `dictionaries/`), and it refuses an `.aff` file that needs anything
else, naming the rule it stopped at, rather than quietly checking such a language wrong.

It was written for [cowrite](https://github.com/superninjv/cowrite), a writing platform, and lives
here on its own.

## What it supports

- UTF-8 dictionaries with single-character flags (no `FLAG` line).
- `PFX` and `SFX` with strip, add and condition; a prefix and a suffix combine when both allow it.
  An affix that carries flags of its own is refused.
- `ICONV` (so the typographic apostrophe is read as `'`), `NOSUGGEST`, `TRY` and `REP`.
- `COMPOUNDRULE` (with `*` only), `COMPOUNDMIN` and `ONLYINCOMPOUND`, used only for ordinals such
  as "21st" and "112th", so compounding is tried only on a word that holds a digit.
- Capitals as Hunspell has them: a word listed in lower case is also right Capitalised or in ALL
  CAPS; one listed capitalised ("Paris") is right in ALL CAPS but not in lower case.
- A hyphenated word is right when each part is. Any other word holding a digit ("x86", "2024")
  passes, since the dictionary cannot judge it.
- A set of the caller's own added words, checked before the dictionary.
- Suggestions: the `REP` table first, then one-edit slips (two letters swapped, an extra letter, a
  missing one, a wrong one) and a missing space, ranked by Hunspell's n-gram closeness. At most 5
  (`MAX_SUGGESTIONS`), in the word's own capitals and kind of apostrophe.

## What it does not

- General compounding (German-style compound words), `FLAG` modes other than single characters,
  affixes with continuation flags, and the other Hunspell directives. These make `parse` return an
  error.
- Hunspell's n-gram suggestion fallback for words more than one edit away. Such a word gets
  few or no suggestions.
- Any language other than English so far; the checker is ready for another whose `.aff` stays
  inside the list above.
- Splitting text into words: the caller does that and asks about one word at a time.

## Use

```rust
use cowrite_spell::Dictionary;
use std::collections::HashSet;

let aff = std::fs::read_to_string("dictionaries/en_US/index.aff")?;
let dic = std::fs::read_to_string("dictionaries/en_US/index.dic")?;
let dictionary = Dictionary::parse(&aff, &dic)?; // Err(message) names an unsupported rule

let added_words = HashSet::new(); // the person's own words, if any
assert!(dictionary.check("unthinkingly", &added_words));
assert!(!dictionary.check("teh", &added_words));
assert_eq!(dictionary.suggest("teh", &added_words)[0], "the");
```

The command-line example reads a dictionary folder and words on stdin:

```
$ echo "teh the colour 21st" | cargo run --release --example check -- dictionaries/en_US
teh: the, tech, tee, tea, ten
colour: color, co lour, col our
```

## Measured

In cowrite, on a book proposal of 2,148 distinct words: all checked in 20 ms in a debug build, and
the dictionary loads in about 70 ms. It flagged the same set of words as Hunspell 1.7.3 on that
text, apart from tokenisation at apostrophes.

## Licence

The code is FSL-1.1-ALv2, copyright 2026 Synoros (`LICENSE.md`). The dictionaries under
`dictionaries/` keep their own licence, MIT AND BSD, in each folder's `license` file.
