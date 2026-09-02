# Data sources and licences for the `hyprpad-osk` prediction model

The model artifact (`en.model`) is **built, not committed** — it is a few
megabytes of derived data, so this directory ships the recipe and the
attribution instead. `build-model.py` downloads every source listed here; the
model file itself carries the attribution string from `ATTRIBUTION.txt` in its
header, so a shipped model always names its own provenance
(`hyprpad-osk-build-model` writes it, `Model::attribution()` reads it back).

Licence analysis follows `docs/research/osk-prediction.md` §4.1, where each
row was verified against the upstream page.

## What the shipped model is made of

| Part | Source | Licence | Obligation |
|---|---|---|---|
| Unigram probabilities | **wordfreq** `large_en`, taken from the [`wordfreq-rs` `models-v1` release assets](https://github.com/kampersanda/wordfreq-rs/releases/tag/models-v1) | **CC BY-SA 4.0** (the data; the code is Apache-2.0) | Attribution **and share-alike** — see below |
| Validity + casing | **SCOWL / Hunspell `en_US`**, from the [LibreOffice dictionaries repository](https://raw.githubusercontent.com/LibreOffice/dictionaries/master/en/en_US.dic) | MIT-like ("Permission to use, copy, modify, distribute and sell … granted without fee") | Attribution |
| Bigram successors | **Leipzig Corpora Collection**, English news (`eng_news_YYYY_1M`), the `-co_n.txt` immediate-neighbour co-occurrence tables | **CC BY 4.0** | Attribution |

**The combined model is therefore CC BY-SA 4.0.** wordfreq's data is
share-alike, and the model is a derivative of it, so a redistributed `en.model`
must carry the attribution and be offered under CC BY-SA 4.0. The hyprpad code
itself stays MIT/Apache-2.0 — the licence applies to the data artifact, not to
the program that reads it.

If a CC BY (attribution-only, no share-alike) model is wanted instead, drop
wordfreq and take unigram probabilities from Google Books 1-grams (CC BY 3.0) or
from HeliBoard's CC BY 4.0 `main_en_US.combined` word list; the pipeline's
`--vocab-size`/word-list stage is the only part that changes. That trade is
open question 1 in the research doc (§10).

### On the Leipzig licence

The Leipzig Corpora Collection's own download page is behind bot protection and
could not be read directly from here (the research doc hit the same wall, §4.1:
"LOD page; primary site blocked"). The CC BY 4.0 reading is corroborated by
HeliBoard's dictionary repository, which builds its English word lists from
"word lists available at https://wortschatz.uni-leipzig.de/en/download/" and
declares them ["source lists under CC BY
4.0"](https://codeberg.org/Helium314/aosp-dictionaries/raw/branch/main/wordlists_experimental/main_en_US.source).
If you need certainty before redistributing a model, confirm the terms on the
Leipzig site, or build with `--books-dir` (below), which is unambiguously
CC BY 3.0.

## The documented upgrade: Google Books Ngrams v3

The research doc's first choice for bigrams is
[Google Books Ngrams v3](https://storage.googleapis.com/books/ngrams/books/datasetsv3.html)
(2020-02-17, **CC BY 3.0**), filtered to `year >= 1980`. It is the better
source — vastly more data, unambiguous licence — and it was **not** used for the
first cut because the English 2-grams are 589 files of ~400 MB each, about
230 GB. `build-model.py --books-dir DIR` reads whatever subset of those files
you have already downloaded, so the upgrade is a download away and no code
change:

```sh
mkdir -p ~/tmp/osk-prediction/books
for i in $(seq -w 0 588); do
  curl -sSLO --output-dir ~/tmp/osk-prediction/books \
    "https://storage.googleapis.com/books/ngrams/books/20200217/eng/2-00$i-of-00589.gz"
done
python3 build-model.py --out en.words --books-dir ~/tmp/osk-prediction/books
```

Books register is bookish rather than conversational (Pechenick et al. 2015 on
its scientific-text drift); the research doc's §4.2 note applies — the personal
cache exists partly to close that gap.

## Sources deliberately not used

| Source | Why not |
|---|---|
| Norvig `count_1w` / `count_2w` | Derived from the LDC-distributed Google Web 1T; the page licenses only the code. Provenance unclear. |
| AOSP `en_US_wordlist.combined` | NOTICE says "Includes Dictionaries © Lexiteria LLC. Used by permission." Not ours to redistribute. |
| OpenSubtitles / OPUS | Best register match, murkiest provenance (the underlying subtitle copyright is unaddressed). |
| Common Crawl / OSCAR, Reddit / Pushshift, COCA, BNC | Terms-of-use caveats, revoked access, commercial, or non-commercial. |
| onboard `en_US.lm` | GPL-3, and only 6,398 bigrams. |

## Attribution text

`ATTRIBUTION.txt` in this directory is what `--attribution-file` writes into the
model header, and what a redistributed model must ship alongside it.
