# cowrite-spell

A small, fast, regularly improved spellchecker in pure Rust, with no dependencies, that reads Hunspell-format dictionaries (`.aff` and `.dic`). It contains no GPL code and no GPL data: the checker and its suggestion
engine are written from scratch, and the only third-party files are the English SCOWL
dictionaries (the `dictionary-en` and `dictionary-en-gb` npm packages, MIT AND BSD), copied under
`dictionaries/`. 

It was written for cowrite, a writing platform, and lives here on its own.

Against public misspelling corpora it puts the intended word first more often than Hunspell 1.7.3
does with the same word list, on every corpus measured: 52.5% of the evaluated misspellings
across six corpora, against 44.1%. It suggests in about a quarter of Hunspell's time and checks
about four times as fast. It also takes about twice as long to load, and its first suggestion
takes a further 35 ms to build what suggesting needs.

Contents: [Results](#results), [Methodology](#methodology), [How it works](#how-it-works),
[The search-structure comparison](#the-search-structure-comparison), [What did not help](#what-did-not-help),
[Future work](#future-work), [What it supports](#what-it-supports), [Use](#use),
[Reproducing](#reproducing), [Licence](#licence).

## Results

Three versions are shown next to Hunspell 1.7.3, all on `dictionaries/en_US`:

- **original**: the checker as it was at commit `16f8311` (Hunspell-style suggestions);
- **slips**: an intermediate version that ranks suggestions by a weighted edit cost
  (a "slip" is a common typing or spelling error, which costs less than an unrelated edit);
- **final**: the current code, which adds a sound-alike layer and the signature search.

Every misspelling is judged against the gold correction of its source. Measured 2026-10-11 on one Linux workstation, r5 5600x. All numbers are printed.

### Summed over all six corpora

| | original | slips | final | Hunspell 1.7.3 |
|---|---|---|---|---|
| top-1 | 44.2% | 52.6% | 52.5% | 44.1% |
| top-5 | 56.2% | 67.6% | 69.8% | 56.1% |
| no suggestion | 0.1% | 0.1% | 0.0% | 0.1% |

### What was counted (final, and Hunspell where it differs)

| corpus | pairs | accepted as a word (not flagged) | correction not in dictionary | evaluated |
|---|---|---|---|---|
| norvig-1 | 265 | 4 | 4 | 257 |
| norvig-2 | 400 | 12 | 14 | 374 |
| birkbeck | 35489 (Hunspell 35489) | 3850 (Hunspell 3892) | 524 (Hunspell 521) | 31115 (Hunspell 31076) |
| holbrook | 1652 (Hunspell 1652) | 583 (Hunspell 587) | 34 (Hunspell 34) | 1035 (Hunspell 1031) |
| aspell | 530 (Hunspell 530) | 15 (Hunspell 16) | 4 (Hunspell 4) | 511 (Hunspell 510) |
| wikipedia | 4309 | 18 | 139 | 4152 |

Where no Hunspell figure is given, it is the same as the final one.

### Top-1 / top-5 / no suggestion, in percent

| corpus | original | slips | final | Hunspell 1.7.3 |
|---|---|---|---|---|
| norvig-1 | 69.6 / 88.3 / 0.0 | 82.9 / 95.3 / 0.0 | 84.8 / 96.1 / 0.0 | 69.6 / 88.3 / 0.0 |
| norvig-2 | 71.7 / 89.3 / 0.0 | 85.3 / 93.9 / 0.0 | 83.7 / 94.9 / 0.0 | 71.7 / 89.3 / 0.0 |
| birkbeck | 38.5 / 50.0 / 0.1 | 46.8 / 62.8 / 0.1 | 46.7 / 65.3 / 0.0 | 38.4 / 49.8 / 0.1 |
| holbrook | 26.8 / 52.5 / 0.0 | 39.5 / 64.9 / 0.0 | 40.0 / 66.2 / 0.0 | 26.1 / 51.8 / 0.0 |
| aspell | 57.3 / 78.1 / 0.0 | 65.9 / 86.7 / 0.0 | 65.2 / 86.5 / 0.0 | 57.6 / 78.0 / 0.0 |
| wikipedia | 85.5 / 96.7 / 0.0 | 93.1 / 98.2 / 0.0 | 93.0 / 98.7 / 0.0 | 85.4 / 96.7 / 0.0 |
| all six summed | 44.2 / 56.2 / 0.1 | 52.6 / 67.6 / 0.1 | 52.5 / 69.8 / 0.0 | 44.1 / 56.1 / 0.1 |

Top-1 against the intermediate "slips" version dips slightly on norvig-2 (85.3 to 83.7) and aspell
(65.9 to 65.2) in the final version; top-5 rises on every corpus except aspell, where it is 86.7
then 86.5. Against the original and against Hunspell, the final version is ahead on every corpus
at both top-1 and top-5.

### Split suggestions among those shown, in percent

A split suggestion cuts the misspelling in two ("co lour"). The corpora's gold corrections are
almost never splits, so a split in the list is mostly a wasted slot.

| corpus | original | slips | final | Hunspell 1.7.3 |
|---|---|---|---|---|
| norvig-1 | 11.5 | 1.1 | 0.2 | 11.5 |
| norvig-2 | 8.6 | 0.7 | 0.1 | 8.6 |
| birkbeck | 8.8 | 0.8 | 0.4 | 8.7 |
| holbrook | 10.5 | 0.9 | 0.3 | 10.4 |
| aspell | 11.4 | 1.2 | 0.7 | 11.4 |
| wikipedia | 9.2 | 1.2 | 0.7 | 9.2 |

### Suggestion latency, p50 / p95 / max in ms

| corpus | original | slips | final | Hunspell 1.7.3 |
|---|---|---|---|---|
| norvig-1 | 5.8 / 8.1 / 9.5 | 3.6 / 4.5 / 5.2 | 3.7 / 5.6 / 7.4 | 10.4 / 13.7 / 14.9 |
| norvig-2 | 6.6 / 8.4 / 8.9 | 3.8 / 4.8 / 5.2 | 3.0 / 5.5 / 7.8 | 11.1 / 14.4 / 17.5 |
| birkbeck | 6.3 / 8.4 / 10.7 | 3.6 / 4.7 / 7.0 | 2.7 / 5.1 / 9.8 | 10.3 / 13.3 / 21.4 |
| holbrook | 3.4 / 7.4 / 9.5 | 2.7 / 4.2 / 5.4 | 2.2 / 4.0 / 5.9 | 5.7 / 11.8 / 15.8 |
| aspell | 6.2 / 8.2 / 9.2 | 3.6 / 4.7 / 5.3 | 2.7 / 5.0 / 6.7 | 10.4 / 13.5 / 17.6 |
| wikipedia | 7.1 / 8.5 / 10.9 | 3.9 / 4.9 / 6.4 | 2.9 / 5.5 / 11.3 | 11.5 / 14.1 / 23.7 |

Over every flagged misspelling in all six corpora together, the final version's latency is
2.7 / 5.1 / 11.3 ms and Hunspell's is 10.4 / 13.4 / 23.7 ms.

### Flags on correct words

| list | original | slips | final | Hunspell 1.7.3 |
|---|---|---|---|---|
| gold corrections, distinct (8376) flagged as misspelled | 332 | 332 | 332 | 336 |
| big.txt distinct words without an apostrophe (34918) flagged | 5419 | 5418 | 5418 | 5418 |
| big.txt distinct lower-case-initial words without an apostrophe (26122) flagged | 3279 | 3279 | 3279 | 3279 |
| flagged by this checker or Hunspell but not both (big.txt, final) | | | 0 | |

The final checker and Hunspell flag exactly the same words in `big.txt`. Of the 8,376 gold
corrections, 332 are flagged as misspelled by the checker (336 by Hunspell). That count did not
change between versions.

### Speed and size

| metric | original | slips | final | Hunspell 1.7.3 |
|---|---|---|---|---|
| dictionary load (median of 5; Hunspell's includes starting its process) | 9.8 ms | 7.1 ms | 21.7 ms | 10.6 ms |
| first suggestion (builds the suggestion index) | 1.7 ms | 1.7 ms | 34.8 ms | - |
| check() over big.txt (1,097,281 words; Hunspell's is `-l` less its load) | 4.24M words/s | 4.31M words/s | 6.54M words/s | 1.74M words/s |
| memory after load (RSS delta; Hunspell's is its process less a one-word-list process) | 11.9 MiB | 1.8 MiB | 2.7 MiB | 4.7 MiB (11.7 MiB whole process) |
| memory once suggestions are built | 12.0 MiB | 1.8 MiB | 7.4 MiB | - |
| `check` example binary | 641 KiB | 649 KiB | 649 KiB | - |
| `src/lib.rs` nonblank lines | 1206 | 1362 | 1158 | - |

What got worse: load time more than doubled against the other versions (9.8 and 7.1 ms to 21.7 ms),
the first suggestion now costs 34.8 ms where it cost 1.7 ms, and memory after load and while
suggesting is above the intermediate version's (2.7 against 1.8 MiB, and 7.4 against 1.8 MiB). The
suggestion index (signatures and sound keys) is built lazily on the first `suggest` call, so a
program that only checks words does not pay the 34.8 ms or the extra memory while suggesting.

## Methodology

### Corpora

Six public misspelling sets [this is only for benchmarking] , a list of (misspelling, correct word) pairs. None is included in this repository.
([Reproducing](#reproducing)).

| name | what it is | source |
|---|---|---|
| norvig-1 | Peter Norvig's development test set (from Mitton's Birkbeck corpus) | norvig.com/spell-testset1.txt |
| norvig-2 | Norvig's second test set | norvig.com/spell-testset2.txt |
| birkbeck | Roger Mitton's Birkbeck spelling error corpus, `missp.dat` | dcs.bbk.ac.uk/~ROGER |
| holbrook | Mitton's Holbrook corpus (schoolchildren's spellings) | dcs.bbk.ac.uk/~ROGER |
| aspell | Mitton's Aspell test set | dcs.bbk.ac.uk/~ROGER |
| wikipedia | Wikipedia's "Lists of common misspellings/For machines" | en.wikipedia.org |

`big.txt` (Norvig's plain-text corpus) is used for check speed and for flags on real text, not for
suggestion accuracy. Each corpus keeps its own terms; they are downloaded by the person running
the harness and not redistributed here. Check them before reusing the data.

### Accuracy

For every pair, the checker is asked whether it flags the misspelling at all.

- A misspelling the checker accepts as a word is counted as "not flagged" and not evaluated: there
  is nothing to suggest for (for example a misspelling that is another real word).
- A pair whose gold correction is not a word in the dictionary is counted as "correction not in
  dictionary (unreachable)" and not evaluated: no checker using that word list could suggest it.
- The rest are **evaluated**. The checker's suggestions (at most 5, `MAX_SUGGESTIONS`) are compared
  with the gold correction (any correction the corpus accepts). **Top-1** is the first suggestion
  being a correct one; **top-5** is one of the five; **no suggestion** is an empty list. All
  percentages are of evaluated pairs. Hunspell's own flags and unreachable counts are taken the same
  way, so the evaluated sets differ by a few pairs between the two checkers (the counts table).

### Latency, load and memory

- Suggest latency is wall time of one `suggest` call per flagged misspelling, after the index is
  built, with p50, p95 and max over the calls. Hunspell's is through its pipe (`hunspell -a`), one
  word at a time, so it includes a pipe round trip of some microseconds.
- Load is the median of 5 loads of the parsed files. Hunspell's includes starting its process.
- Memory is the resident-set growth of the process around the first load, and then around the first
  suggestion. It is measured after one load: an earlier version of the harness measured after five
  loads, and a load's freed working memory stays with the process, which counted more than one
  dictionary. That bug is fixed. Hunspell's memory is its process with the word list less its
  process with a one-word list.
- Check speed is words per second over every word of `big.txt` (1,097,281). Hunspell's is its `-l`
  mode less its load.
- Flags compare words without an apostrophe, because Hunspell's own tokenizer splits "couldn't"
  into "couldn" before judging it.

### Tuning discipline

The edit weights were tuned on three corpora (norvig-1, aspell, wikipedia) with the other three
(norvig-2, birkbeck, holbrook) held out. The held-out corpora improved alike, which is the
evidence the weights are not fitted to the tuning sets. Later choices were not made that cleanly:
the cost of a split, the cost of a sound-alike, how many signatures are scored, and the sound-alike
layer itself were chosen watching all six corpora. The final numbers on the held-out corpora are
therefore a little less independent than the intermediate ones. The weights are hand-set, not
learned, and are named constants with provenance in `src/lib.rs`.

One behaviour fix came out of the comparison: an ALL CAPS form of a mixed-case dictionary word
("MCKINLEY" for "McKinley") is accepted, as Hunspell does.

## How it works

- **Checking.** Every affix rule is applied once, at load, so the dictionary is held as its word
  forms (about 124,000 for American English) packed end to end in one string with a hash table
  over them. Checking a word is one hash lookup.
- **Finding candidates**, four ways:
  - *near spellings*: each form has a 64-bit signature (which letters it holds, a hash of which
    letter pairs, and a length class); the 1,000 forms whose signatures differ from the
    misspelling's in the fewest bits are taken. A plain scan of all signatures (XOR and bit count)
    is about 0.1 ms;
  - *sound-alikes*: each form has a sounds-like key, a small English reduction written here in
    the spirit of Metaphone ("something" and "sumthing" share one); the forms whose key is the
    misspelling's, or one edit from it, are taken;
  - the `.aff`'s listed common misspellings (`REP`) applied where they occur;
  - the word split in two by a missing space ("a lot", "well known").
- **Ranking**, by one cost: a weighted edit distance in which a missing letter (about half an
  edit), a vowel for a vowel, a doubled letter, a swap, a neighbouring key (`KEY`) and a listed
  misspelling cost less than an unrelated letter, and a wrong first letter costs more. A split costs
  two edits, and is never offered with a one-letter piece or with a second half that is one of the
  dictionary's suffixes ("post ion", "admit ed"). A sound-alike costs at most two edits; a name
  offered for a lower-case word, or an apostrophe the word lacked, costs a little more.
- The suggestion structures are built on the first `suggest` call, so a dictionary used only to
  check words loads faster and is smaller.

## The search-structure comparison

Ranking and candidate search are separable, so before settling on the signature scan, three ways
of finding candidates were prototyped and run through the same ranking on all six corpora, with and
without the sound-alike layer. The table is top-1 / top-5 in percent. Suggest latency is p50 in the
prototype harness and includes tie-break costs; prototype load time and memory are not comparable
with the shipped numbers.

| candidates from | norvig-1 | norvig-2 | birkbeck | holbrook | aspell | wikipedia | p50 |
|---|---|---|---|---|---|---|---|
| slips (the intermediate version) | 82.9 / 95.3 | 85.3 / 93.9 | 46.8 / 62.8 | 39.2 / 64.8 | 65.9 / 86.7 | 93.1 / 98.2 | 3.6 ms |
| slips + sound-alike | 82.9 / 98.1 | 84.8 / 94.9 | 47.3 / 65.5 | 39.1 / 66.1 | 65.9 / 87.7 | 93.4 / 98.7 | 5.2 ms |
| A: signatures | 84.8 / 95.7 | 84.5 / 95.5 | 46.3 / 63.4 | 39.8 / 65.3 | 64.8 / 85.5 | 93.1 / 98.7 | 3.9 ms |
| **A + sound-alike (kept)** | 84.8 / 96.1 | 84.2 / 94.9 | 46.7 / 65.3 | 40.0 / 66.2 | 65.4 / 86.3 | 93.0 / 98.7 | 2.6 ms after optimizing |
| B: automaton (weighted best-first search over the expanded forms) | 82.5 / 93.4 | 81.6 / 89.6 | 39.5 / 49.2 | 39.6 / 61.4 | 60.3 / 72.2 | 91.7 / 96.4 | 10.6 ms |
| B + sound-alike | 82.5 / 95.7 | 83.2 / 94.1 | 44.8 / 62.6 | 40.1 / 65.5 | 63.8 / 82.0 | 92.6 / 98.2 | 11.1 ms |
| C: BK-tree (a VP-tree gave identical results) | 81.3 / 94.6 | 82.1 / 90.9 | 38.7 / 48.9 | 39.0 / 62.4 | 59.7 / 74.4 | 91.5 / 96.7 | 3.6 ms (VP-tree 4.2 ms) |
| C + sound-alike | 82.1 / 96.5 | 84.8 / 94.7 | 44.9 / 62.0 | 39.4 / 64.6 | 64.0 / 82.0 | 92.5 / 98.1 | 4.2 ms |

The metric trees search by plain edit distance. How much of the dictionary they visit: at distance 1, the BK-tree visits about 2,236 forms (1.8%) and the VP-tree
3,440 (2.8%); at distance 2, the BK-tree visits 17,058 (14%) and the VP-tree 27,330 (22%). Multi-index
hashing over the signatures gave exactly the same suggestions as the plain scan, but was slower
(2.8 against 2.6 ms) and 3.5 MiB larger, so the plain scan stayed.

The conclusion is that the gain over Hunspell comes from the ranking and the sound layer, not from
the search structure. Structure A is the best or level on most corpora and the fastest; B and C, which search harder
by edit distance, did worse on birkbeck, aspell and norvig-2 and were no faster.

## What did not help

- More n-gram stems for candidate search: +1.2 top-5 on norvig-1 at +20% latency.
- A split cost of 250 instead of 200.
- A sound-alike cost of 150.
- Scoring only the 300 nearest signatures instead of 1,000: about 1 point lower top-5.
- A word-frequency prior at step 30: 0.9 lower top-1 on wikipedia (step 20 helped; see below).
- Multi-index hashing over signatures (see the comparison).

## Future work

The largest remaining gain measured is a word-frequency prior: preferring the more common of two
equally close words, using SCOWL's size classes. At step 20 it moved top-1 as follows.

| corpus | top-1 now | top-1 with prior |
|---|---|---|
| holbrook | 40.0% | 50.1% |
| birkbeck | 46.7% | 52.2% |
| aspell | 65.4% | 71.6% |
| norvig-2 | 84.2% | 89.3% |

It is not shipped, because the `.dic` files do not carry the size classes and the prior needs a new
data file alongside the dictionary. It is parked. (The "now" column is the experiment's baseline, the signature search with the sound layer at
the time; aspell and norvig-2 differ from the final table by 0.2 and 0.5.)

Also open: the words flagged as misspelled although they are gold corrections (332 of 8,376) are
unchanged from the original version.

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

## Reproducing

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

It runs `hunspell` (1.7.3 for the table above) from `PATH` (or `$HUNSPELL`) with
`-d dictionaries/en_US/index`; pass `--no-hunspell` to measure this checker alone,
`--only norvig-1,aspell` to run some corpora, and `--dump <file>` to write every misspelling with
both checkers' suggestions. With Hunspell it takes about half an hour, nearly all of it Hunspell's
suggestions. The "original" and "slips" columns come from running the same harness on those earlier
versions of the code; the comparison of search structures was run on prototypes that are not in
the tree.

## Licence

The code is FSL-1.1-ALv2, copyright 2026 Synoros (see `LICENSE.md`). The dictionaries under
`dictionaries/` keep their own licence, MIT AND BSD, in each folder's `license` file.
