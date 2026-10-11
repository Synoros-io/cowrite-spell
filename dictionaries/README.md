# dictionaries

Hunspell dictionaries, one folder per language, each holding `index.aff` (the affix rules),
`index.dic` (the stems and their rule flags) and `license`.

## What is here

Both are the SCOWL-based English dictionaries (wordlist.aspell.net) as the npm packages publish
them, copied unmodified out of the registry tarballs, which were checked against the registry's
`dist.integrity` before copying. Licence: MIT AND BSD (the full text, with SCOWL's own notices, is
each folder's `license`). They keep that licence; it is not the licence of the code in this repo.

| folder | package | tarball | integrity (SHA-512, base64) |
|---|---|---|---|
| `en_US` | `dictionary-en` 4.0.0 | https://registry.npmjs.org/dictionary-en/-/dictionary-en-4.0.0.tgz | `3NHnE1uq33ZE/CIwaZ6gqxa4BnglHnxeAcTM0GJ7cmtRGcvX9InMK/IqLtYcUMFCUpUNgybH+DzkqdgAo3F1zg==` |
| `en_GB` | `dictionary-en-gb` 3.0.0 | https://registry.npmjs.org/dictionary-en-gb/-/dictionary-en-gb-3.0.0.tgz | `gU/dwC9xGYPh/0CCcmGNXO8OwSgOySAqevcSXxADCsgUD+tr6OJ52u7XaC7No7dNssvdUSfVjkzcqLrRZq3kPA==` |

From each tarball only `package/index.aff`, `package/index.dic` and `package/license` were kept.
The two `.aff` files are identical; the `.dic` files differ in spelling (colour, favourite).

## Adding a language, or shipping fewer

A new folder named for the language (`de_DE`) with its `index.aff`, `index.dic` and `license`
is all it takes, provided its `.aff` uses only what the checker implements: UTF-8, single-character
flags, `ICONV`, `REP`, `KEY`, `NOSUGGEST`, `NOSPLITSUGS`, `PFX`/`SFX` without continuation flags,
and `COMPOUNDRULE` with `*` only (the header of `src/lib.rs` lists them). The checker refuses an
`.aff` that asks for more and names the rule it stopped at, rather than checking wrongly. Add a row
above with its source and integrity. Delete a folder to ship without that language. Suggestions
for another language would also want its own sounds-like key: the one in `src/lib.rs` is English.
