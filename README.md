# cowrite-spell

A small Hunspell-format spellchecker in pure Rust, with no dependencies. It reads the English
SCOWL dictionaries (the `dictionary-en` and `dictionary-en-gb` npm packages, copied here under
`dictionaries/`), and it refuses an `.aff` file that needs anything it does not implement, naming
the rule it stopped at, rather than quietly checking such a language wrong.

It was written for [cowrite](https://github.com/superninjv/cowrite), a writing platform, and lives
here on its own.

Against public misspelling corpora it puts the intended word first more often than Hunspell 1.7
does with the same word list, on every corpus measured. It suggests in about a quarter of
Hunspell's time and checks about four times as fast, but takes about twice as long to load, and
its first suggestion takes a further 35 ms to build what suggesting needs. The table and the
command that measures it are under [Measured](#measured).

## How it works

- **Checking.** Every affix rule is applied once, at load, so the dictionary is held as its word
  forms (about 124,000 for American English) packed end to end in one string with a hash table
  over them. Checking a word is one hash lookup.
- **Finding candidates**, three ways:
  - *near spellings*: each form has a 64-bit signature (which letters it holds, a hash of which
    letter pairs, and a length class); the 1,000 forms whose signatures differ from the
    misspelling's in the fewest bits are taken. A scan of all signatures (XOR and bit count) is
    about 0.1 ms;
  - *sound-alikes*: each form has a sounds-like key, a small English reduction written here in
    the spirit of Metaphone ("something" and "sumthing" share one); the forms whose key is the
    misspelling's, or one edit from it, are taken;
  - the `.aff`'s listed common misspellings (`REP`) applied where they occur, and the word split
    in two by a missing space ("a lot", "well known"), never into a fragment ("co lour", "post ion").
- **Ranking**, by one cost: a weighted edit distance in which a missing letter, a vowel for a
  vowel, a doubled letter, a swap, a neighbouring key (`KEY`) and a listed misspelling cost less
  than an unrelated letter, and a wrong first letter costs more; a sound-alike costs at most two
  edits; a name offered for a lower-case word, or an apostrophe the word lacked, costs a little
  more. The weights were tuned on three of the corpora and checked on the other three.
- The suggestion structures are built on the first `suggest` call, so a dictionary used only to
  check words loads faster and is smaller.

## What it supports

- UTF-8 dictionaries with single-character flags (no `FLAG` line).
- `PFX` and `SFX` with strip, add and condition; a prefix and a suffix combine when both allow it.
  An affix that carries flags of its own is refused.
- `ICONV` (so the typographic apostrophe is read as `'`), `NOSUGGEST`, `REP`, `KEY` and
  `NOSPLITSUGS`. `TRY`, `MAXNGRAMSUGS`, `MAXDIFF` and `ONLYMAXDIFF` tune Hunspell's own search and
  are read past.
- `COMPOUNDRULE` (with `*` only), `COMPOUNDMIN` and `ONLYINCOMPOUND`, used only for ordinals such
  as "21st" and "112th", so compounding is tried only on a word that holds a digit.
- Capitals as Hunspell has them: a word listed in lower case is also right Capitalised or in ALL
  CAPS; one listed capitalised ("Paris") is right in ALL CAPS but not in lower case; one with
  capitals inside ("McKinley") is right as listed or in ALL CAPS.
- A hyphenated word is right when each part is. Any other word holding a digit ("x86", "2024")
  passes, since the dictionary cannot judge it.
- A set of the caller's own added words, checked before the dictionary and offered as suggestions.
- At most 5 suggestions (`MAX_SUGGESTIONS`), in the word's own capitals and kind of apostrophe.

## What it does not

- General compounding (German-style compound words), `FLAG` modes other than single characters,
  affixes with continuation flags, and the other Hunspell directives. These make `parse` return an
  error.
- Hunspell's suggestions: the lists are its own, ranked differently on purpose.
- Word frequency. The shipped dictionaries carry none (SCOWL's size classes, its frequency bands,
  are not in the `.dic`), so two words equally close to a misspelling are not told apart by how
  common they are.
- Any language other than English so far: the sounds-like key and the vowel costs are English.
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
$ echo "teh the colour 21st 22th sumthing alot" | cargo run --release --example check -- dictionaries/en_US
teh: the, tech, thee, teeth, tee
colour: color, cooler, colors, contour, collar
22th: 22nd
sumthing: something, soothing, seething, somethings, smoothing
alot: a lot, allot, alto, aloft, allots
```

## Measured

Measured 2026-10-11 on one Linux workstation, this checker and Hunspell 1.7.3 both using
`dictionaries/en_US`. Every misspelling is judged against the gold correction its corpus gives, not
against Hunspell's answer. "Evaluated" counts the misspellings the checker flags whose correction
the dictionary holds; the others are counted in their own rows rather than as misses.

Summed over all six corpora, the first suggestion is the intended word for 52.5% of the evaluated
misspellings (Hunspell 44.1%, this library's previous version 44.2%) and one of the five for 69.8%
(Hunspell 56.1%, previous version 56.2%).

#### Suggestion quality against gold corrections

| corpus | metric | cowrite-spell | Hunspell 1.7.3 |
|---|---|---|---|
| norvig-1 | pairs (misspelling, correction) | 265 | 265 |
| norvig-1 | misspelling accepted as a word (not flagged) | 4 | 4 |
| norvig-1 | correction not in dictionary (unreachable) | 4 | 4 |
| norvig-1 | evaluated | 257 | 257 |
| norvig-1 | top-1 accuracy | 84.8% | 69.6% |
| norvig-1 | top-5 accuracy | 96.1% | 88.3% |
| norvig-1 | no suggestion | 0.0% | 0.0% |
| norvig-1 | split suggestions among those shown ("co lour") | 0.2% | 11.5% |
| norvig-1 | suggest latency p50 / p95 / max | 3.7 ms / 5.6 ms / 7.4 ms | 10.4 ms / 13.7 ms / 14.9 ms |
| norvig-2 | pairs (misspelling, correction) | 400 | 400 |
| norvig-2 | misspelling accepted as a word (not flagged) | 12 | 12 |
| norvig-2 | correction not in dictionary (unreachable) | 14 | 14 |
| norvig-2 | evaluated | 374 | 374 |
| norvig-2 | top-1 accuracy | 83.7% | 71.7% |
| norvig-2 | top-5 accuracy | 94.9% | 89.3% |
| norvig-2 | no suggestion | 0.0% | 0.0% |
| norvig-2 | split suggestions among those shown ("co lour") | 0.1% | 8.6% |
| norvig-2 | suggest latency p50 / p95 / max | 3.0 ms / 5.5 ms / 7.8 ms | 11.1 ms / 14.4 ms / 17.5 ms |
| birkbeck | pairs (misspelling, correction) | 35489 | 35489 |
| birkbeck | misspelling accepted as a word (not flagged) | 3850 | 3892 |
| birkbeck | correction not in dictionary (unreachable) | 524 | 521 |
| birkbeck | evaluated | 31115 | 31076 |
| birkbeck | top-1 accuracy | 46.7% | 38.4% |
| birkbeck | top-5 accuracy | 65.3% | 49.8% |
| birkbeck | no suggestion | 0.0% | 0.1% |
| birkbeck | split suggestions among those shown ("co lour") | 0.4% | 8.7% |
| birkbeck | suggest latency p50 / p95 / max | 2.7 ms / 5.1 ms / 9.8 ms | 10.3 ms / 13.3 ms / 21.4 ms |
| holbrook | pairs (misspelling, correction) | 1652 | 1652 |
| holbrook | misspelling accepted as a word (not flagged) | 583 | 587 |
| holbrook | correction not in dictionary (unreachable) | 34 | 34 |
| holbrook | evaluated | 1035 | 1031 |
| holbrook | top-1 accuracy | 40.0% | 26.1% |
| holbrook | top-5 accuracy | 66.2% | 51.8% |
| holbrook | no suggestion | 0.0% | 0.0% |
| holbrook | split suggestions among those shown ("co lour") | 0.3% | 10.4% |
| holbrook | suggest latency p50 / p95 / max | 2.2 ms / 4.0 ms / 5.9 ms | 5.7 ms / 11.8 ms / 15.8 ms |
| aspell | pairs (misspelling, correction) | 530 | 530 |
| aspell | misspelling accepted as a word (not flagged) | 15 | 16 |
| aspell | correction not in dictionary (unreachable) | 4 | 4 |
| aspell | evaluated | 511 | 510 |
| aspell | top-1 accuracy | 65.2% | 57.6% |
| aspell | top-5 accuracy | 86.5% | 78.0% |
| aspell | no suggestion | 0.0% | 0.0% |
| aspell | split suggestions among those shown ("co lour") | 0.7% | 11.4% |
| aspell | suggest latency p50 / p95 / max | 2.7 ms / 5.0 ms / 6.7 ms | 10.4 ms / 13.5 ms / 17.6 ms |
| wikipedia | pairs (misspelling, correction) | 4309 | 4309 |
| wikipedia | misspelling accepted as a word (not flagged) | 18 | 18 |
| wikipedia | correction not in dictionary (unreachable) | 139 | 139 |
| wikipedia | evaluated | 4152 | 4152 |
| wikipedia | top-1 accuracy | 93.0% | 85.4% |
| wikipedia | top-5 accuracy | 98.7% | 96.7% |
| wikipedia | no suggestion | 0.0% | 0.0% |
| wikipedia | split suggestions among those shown ("co lour") | 0.7% | 9.2% |
| wikipedia | suggest latency p50 / p95 / max | 2.9 ms / 5.5 ms / 11.3 ms | 11.5 ms / 14.1 ms / 23.7 ms |
| all six, summed | top-1 / top-5 / none | 52.5% / 69.8% / 0.0% | 44.1% / 56.1% / 0.1% |

#### Flags on correct words

| list | metric | cowrite-spell | Hunspell 1.7.3 |
|---|---|---|---|
| gold corrections, distinct (8376) | flagged as misspelled | 332 | 336 |
| big.txt distinct words without an apostrophe (34918) | flagged | 5418 | 5418 |
| big.txt distinct words without an apostrophe | flagged by one checker only | 0 | 0 |
| big.txt distinct lower-case-initial words without an apostrophe (26122) | flagged | 3279 | 3279 |
| big.txt distinct lower-case-initial words without an apostrophe | flagged by one checker only | 0 | 0 |

#### Speed and size

| metric | cowrite-spell | Hunspell 1.7.3 |
|---|---|---|
| dictionary load (median of 5; Hunspell's includes starting its process) | 21.7 ms | 10.6 ms |
| check() over big.txt (1097281 words; Hunspell's is `-l` less its load) | 6543282 words/s | 1735890 words/s |
| suggest latency p50 / p95 / max, every flagged misspelling (Hunspell's through its pipe) | 2.7 ms / 5.1 ms / 11.3 ms | 10.4 ms / 13.4 ms / 23.7 ms |
| memory after load (RSS delta; Hunspell's is its process less its process with a one-word list) | 2.7 MiB | 4.7 MiB (11.7 MiB whole process) |
| first suggestion, which builds the suggestion index | 34.8 ms | - |
| memory once suggestions are built (RSS delta) | 7.4 MiB | - |
| check example binary (this build's profile) | 649 KiB | - |
| src/lib.rs nonblank lines | 1158 | - |

Notes on reading it:
- The edit weights were tuned on Norvig's first set, Aspell's and Wikipedia's lists, with Norvig's
  second set, Birkbeck and Holbrook held out, which improved alike. Later choices (the cost of a
  split, the cost of a sound-alike, how many signatures are scored) were made watching all six.
- Birkbeck and Holbrook are schoolchildren's spellings, often far from the word ("sumthing",
  "becos"). Many of the remaining misses there need knowing which word is common, which the
  dictionaries do not carry.
- "Split suggestions" counts suggestions that only cut the misspelling in two ("co lour").
- Hunspell's latency is through its pipe (`hunspell -a`), one word at a time, so it includes a
  pipe round trip of some microseconds. Its load includes starting its process. Its memory is its
  process less the same process with a one-word dictionary.
- Flags are compared on words without an apostrophe, because Hunspell's own tokenizer splits
  "couldn't" into "couldn" before judging it. On those, the two checkers flag exactly the same
  words.
- "First suggestion" is the one-time cost of building the suggestion structures, paid on the first
  `suggest` call.

### Reproducing

The corpora are public; download them into a folder of their own and run the harness:

```
mkdir -p .scratch/data && cd .scratch/data
curl -LO https://norvig.com/spell-testset1.txt
curl -LO https://norvig.com/spell-testset2.txt
curl -LO https://norvig.com/big.txt
curl -LO https://www.dcs.bbk.ac.uk/~ROGER/missp.dat
curl -LO https://www.dcs.bbk.ac.uk/~ROGER/holbrook-missp.dat
curl -LO https://www.dcs.bbk.ac.uk/~ROGER/aspell.dat
curl -L -o wikipedia-for-machines.txt 'https://en.wikipedia.org/w/index.php?title=Wikipedia:Lists_of_common_misspellings/For_machines&action=raw'
cd ../..
cargo build --release --examples
./target/release/examples/measure dictionaries/en_US .scratch/data
```

It runs `hunspell` from `PATH` (or `$HUNSPELL`) with `-d dictionaries/en_US/index`; pass
`--no-hunspell` to measure this checker alone, `--only norvig-1,aspell` to run some corpora, and
`--dump <file>` to write every misspelling with both checkers' suggestions. With Hunspell it takes
about half an hour, nearly all of it Hunspell's suggestions.

## Licence

The code is FSL-1.1-ALv2, copyright 2026 Synoros (`LICENSE.md`). The dictionaries under
`dictionaries/` keep their own licence, MIT AND BSD, in each folder's `license` file.
