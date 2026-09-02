#!/usr/bin/env python3
"""Build the hyprpad-osk prediction model's word list from freely licensed data.

This is the *data* half of the pipeline described in
`docs/research/osk-prediction.md` §4.2. It downloads (and caches) the sources,
merges them, and writes the plain-text word list that
`hyprpad-osk-build-model` turns into the binary model:

    python3 build-model.py --out ~/tmp/osk-prediction/en.words
    cargo run --release --bin hyprpad-osk-build-model -- \\
        ~/tmp/osk-prediction/en.words ~/.local/share/hyprpad-osk/en.model \\
        --attribution-file osk/tools/build-model/ATTRIBUTION.txt

Nothing here runs at build time or at run time: the model is an artifact you
build once. See DATA-LICENSES.md for what each source is and what its licence
requires.

Sources (all fetched over HTTPS, all cached under --cache):

  unigrams   wordfreq `large_en` (CC BY-SA 4.0 data, Apache-2.0 code), taken
             from the wordfreq-rs `models-v1` release assets so `cargo build`
             never touches the network. `word probability` per line.
  validity   Hunspell/SCOWL en_US `.dic` from the LibreOffice dictionaries
             repository (MIT-like). Supplies the casing a lower-cased frequency
             list loses ("I", proper nouns) and, with --scowl-only, a spelling
             filter.
  bigrams    Leipzig Corpora Collection (CC BY 4.0) `*-co_n.txt`: precomputed
             counts of *immediate neighbour* co-occurrences — i.e. bigrams —
             with `*-words.txt` giving the id → word map and each word's count.
             Several corpora can be merged (--corpus, repeatable).

The research doc's first choice for bigrams is Google Books Ngrams v3 (CC BY
3.0), which is ~230 GB for English 2-grams alone and impractical as a first cut;
--books-dir reads a locally downloaded subset of it instead, and is the
documented upgrade path. See DATA-LICENSES.md.
"""

import argparse
import collections
import gzip
import os
import sys
import tarfile
import urllib.request

WORDFREQ_URL = (
    "https://github.com/kampersanda/wordfreq-rs/releases/download/models-v1/large_en.txt.zst"
)
SCOWL_DIC_URL = "https://raw.githubusercontent.com/LibreOffice/dictionaries/master/en/en_US.dic"
LEIPZIG_BASE = "https://downloads.wortschatz-leipzig.de/corpora"

# The corpora merged by default: four years of English news plus Wikipedia,
# ~1.4 GB of downloads for ~915 k in-vocabulary bigram types. More years (or
# more registers) is the cheap way to add coverage; --corpus replaces this list.
DEFAULT_CORPORA = [
    "eng_news_2023_1M",
    "eng_news_2020_1M",
    "eng_news_2019_1M",
    "eng_news_2016_1M",
    "eng_wikipedia_2016_1M",
]

# A token we will never put in the lexicon, whatever the source says.
MAX_WORD_CHARS = 30


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", required=True, help="word list to write")
    ap.add_argument("--cache", default=os.path.expanduser("~/tmp/osk-prediction/cache"),
                    help="download cache directory (default: %(default)s)")
    ap.add_argument("--vocab-size", type=int, default=60000,
                    help="maximum words in the lexicon (default: %(default)s)")
    ap.add_argument("--successors", type=int, default=32,
                    help="maximum successors stored per word (default: %(default)s)")
    ap.add_argument("--min-bigram-count", type=int, default=3,
                    help="drop bigrams seen fewer times than this (default: %(default)s)")
    ap.add_argument("--corpus", action="append", default=None, metavar="NAME",
                    help="Leipzig corpus to merge, repeatable (default: %s)"
                         % " ".join(DEFAULT_CORPORA))
    ap.add_argument("--books-dir", default=None, metavar="DIR",
                    help="use Google Books Ngrams v3 2-gram .gz files in DIR instead of "
                         "the Leipzig corpora (the upgrade path; see DATA-LICENSES.md)")
    ap.add_argument("--books-min-year", type=int, default=1980,
                    help="ignore Google Books rows before this year (default: %(default)s)")
    ap.add_argument("--scowl-only", action="store_true",
                    help="keep only words SCOWL/Hunspell knows (a stricter, smaller lexicon)")
    ap.add_argument("--offline", action="store_true",
                    help="fail rather than download anything not already cached")
    args = ap.parse_args(argv)
    if args.corpus is None:
        args.corpus = list(DEFAULT_CORPORA)

    os.makedirs(args.cache, exist_ok=True)

    log("reading wordfreq large_en")
    freqs = read_wordfreq(fetch(WORDFREQ_URL, args.cache, "large_en.txt.zst", args.offline))
    log("  %d entries" % len(freqs))

    log("reading SCOWL/Hunspell en_US")
    scowl = read_scowl(fetch(SCOWL_DIC_URL, args.cache, "en_US.dic", args.offline))
    log("  %d stems" % len(scowl))

    vocab = build_vocab(freqs, scowl, args.vocab_size, args.scowl_only)
    log("vocabulary: %d words" % len(vocab))

    if args.books_dir:
        pairs, totals = read_books_bigrams(args.books_dir, vocab, args.books_min_year,
                                           args.min_bigram_count)
    else:
        pairs, totals = read_leipzig_bigrams(args.corpus, args.cache, vocab,
                                             args.min_bigram_count, args.offline)
    log("bigrams: %d pairs over %d distinct first words" % (len(pairs), len(totals)))

    write_wordlist(args.out, vocab, pairs, totals, args.successors)
    return 0


# ---------------------------------------------------------------------------
# Sources
# ---------------------------------------------------------------------------


def fetch(url, cache, name, offline):
    """Download `url` into the cache once; return the local path."""
    path = os.path.join(cache, name)
    if os.path.exists(path) and os.path.getsize(path) > 0:
        return path
    if offline:
        die("%s is not cached and --offline was given" % name)
    log("downloading %s" % url)
    tmp = path + ".part"
    with urllib.request.urlopen(url) as r, open(tmp, "wb") as f:
        while True:
            chunk = r.read(1 << 20)
            if not chunk:
                break
            f.write(chunk)
    os.replace(tmp, path)
    return path


def read_wordfreq(path):
    """wordfreq's `word probability` text, zstd-compressed."""
    try:
        import zstandard
    except ImportError:
        die("python-zstandard is needed to read the wordfreq asset "
            "(pacman -S python-zstandard, or pip install zstandard)")
    with open(path, "rb") as f:
        data = zstandard.ZstdDecompressor().stream_reader(f).read()
    out = {}
    for line in data.decode("utf-8").splitlines():
        word, _, p = line.partition(" ")
        if not p:
            continue
        try:
            out[word] = float(p)
        except ValueError:
            continue
    return out


def read_scowl(path):
    """Hunspell `.dic` stems, keeping the casing the dictionary spells them with."""
    out = set()
    with open(path, "r", encoding="utf-8", errors="replace") as f:
        first = True
        for line in f:
            if first:  # the count header
                first = False
                continue
            stem = line.split("/", 1)[0].strip()
            if stem:
                out.add(stem)
    return out


# ---------------------------------------------------------------------------
# Vocabulary
# ---------------------------------------------------------------------------


def shapely(word):
    """Whether a token may be a lexicon word at all: letters, apostrophes and
    inner hyphens only, no digits, no symbols, a sane length."""
    if not word or len(word) > MAX_WORD_CHARS:
        return False
    if not (word[0].isalpha() and word[-1].isalpha()):
        return False
    return all(c.isalpha() or c in "'-" for c in word)


def build_vocab(freqs, scowl, limit, scowl_only):
    """The lexicon: the commonest wordfreq entries that pass the shape filter,
    plus the capitalised spelling SCOWL knows for any of them (a lower-cased
    frequency list has no "I", no "Wayland", no "London").

    Returns an ordered dict word -> probability, commonest first.
    """
    # `sorted`, not the set's own order: Python randomises string hashing per
    # process, and two SCOWL stems can fold to the same lower-case key, so
    # iterating the set directly would make the build non-reproducible.
    scowl_lower = {}
    for stem in sorted(scowl):
        if any(c.isupper() for c in stem) and shapely(stem):
            scowl_lower.setdefault(stem.lower(), stem)

    candidates = []
    for word, p in freqs.items():
        if not shapely(word):
            continue
        if scowl_only and word not in scowl and word.capitalize() not in scowl:
            continue
        candidates.append((p, word))
        cap = scowl_lower.get(word)
        if cap and cap != word:
            # A proper noun the frequency list only knows lower-cased. Give the
            # capitalised spelling a quarter of the probability: it is a real
            # word people type, but the lower-cased form covers most uses.
            candidates.append((p * 0.25, cap))
    candidates.sort(key=lambda t: (-t[0], t[1]))
    out = {}
    for p, word in candidates:
        if word in out:
            continue
        out[word] = p
        if len(out) >= limit:
            break
    return out


# ---------------------------------------------------------------------------
# Bigrams
# ---------------------------------------------------------------------------


def read_leipzig_bigrams(corpora, cache, vocab, min_count, offline):
    """Merge `*-co_n.txt` (immediate-neighbour co-occurrence counts) from one or
    more Leipzig corpora.

    Returns (pairs, totals): pairs[(a, b)] = count, totals[a] = count of `a` as
    a first word, both restricted to the vocabulary.
    """
    pairs = collections.Counter()
    totals = collections.Counter()
    for name in corpora:
        path = fetch("%s/%s.tar.gz" % (LEIPZIG_BASE, name), cache, name + ".tar.gz", offline)
        log("reading %s" % name)
        words, cooc = None, None
        with tarfile.open(path, "r:gz") as tar:
            for member in tar.getmembers():
                if member.name.endswith("-words.txt"):
                    words = read_leipzig_words(tar.extractfile(member))
                elif member.name.endswith("-co_n.txt"):
                    cooc = member
            if words is None or cooc is None:
                die("%s has no -words.txt / -co_n.txt" % name)
            n = 0
            for raw in tar.extractfile(cooc):
                cols = raw.decode("utf-8", "replace").rstrip("\n").split("\t")
                if len(cols) < 3:
                    continue
                try:
                    a, b, c = int(cols[0]), int(cols[1]), int(cols[2])
                except ValueError:
                    continue
                wa, wb = words.get(a), words.get(b)
                if wa is None or wb is None:
                    continue
                wa, wb = normalize(wa, vocab), normalize(wb, vocab)
                if wa is None or wb is None:
                    continue
                pairs[(wa, wb)] += c
                totals[wa] += c
                n += 1
            log("  %d in-vocabulary pairs" % n)
    if min_count > 1:
        for key in [k for k, c in pairs.items() if c < min_count]:
            totals[key[0]] -= pairs.pop(key)
    return pairs, totals


def read_leipzig_words(fh):
    """`id \\t word \\t frequency` -> {id: word}."""
    out = {}
    for raw in fh:
        cols = raw.decode("utf-8", "replace").rstrip("\n").split("\t")
        if len(cols) < 2:
            continue
        try:
            out[int(cols[0])] = cols[1]
        except ValueError:
            continue
    return out


def read_books_bigrams(directory, vocab, min_year, min_count):
    """Google Books Ngrams v3 English 2-grams, from `.gz` files already
    downloaded into `directory` (the doc's §4.2 recipe, filtered to year >=
    min_year). Rows are `ngram TAB year,match_count,volume_count TAB ...`."""
    pairs = collections.Counter()
    totals = collections.Counter()
    names = sorted(n for n in os.listdir(directory) if n.endswith(".gz"))
    if not names:
        die("no .gz 2-gram files in %s" % directory)
    for name in names:
        log("reading %s" % name)
        with gzip.open(os.path.join(directory, name), "rt", encoding="utf-8",
                       errors="replace") as f:
            for line in f:
                head, _, rest = line.partition("\t")
                toks = head.split(" ")
                if len(toks) != 2:
                    continue
                a = normalize(toks[0].split("_")[0], vocab)
                b = normalize(toks[1].split("_")[0], vocab)
                if a is None or b is None:
                    continue
                total = 0
                for chunk in rest.rstrip("\n").split("\t"):
                    bits = chunk.split(",")
                    if len(bits) != 3:
                        continue
                    try:
                        year, count = int(bits[0]), int(bits[1])
                    except ValueError:
                        continue
                    if year >= min_year:
                        total += count
                if total:
                    pairs[(a, b)] += total
                    totals[a] += total
    if min_count > 1:
        for key in [k for k, c in pairs.items() if c < min_count]:
            totals[key[0]] -= pairs.pop(key)
    return pairs, totals


def normalize(token, vocab):
    """Map a corpus token onto a vocabulary word, or None.

    Tried as written (so "Wayland" stays "Wayland"), then lower-cased (so a
    sentence-initial "The" counts towards "the", which is what a keyboard
    predicting mid-sentence wants).
    """
    if token in vocab:
        return token
    lower = token.lower()
    if lower in vocab:
        return lower
    return None


# ---------------------------------------------------------------------------
# Output
# ---------------------------------------------------------------------------


def write_wordlist(path, vocab, pairs, totals, max_successors):
    by_first = collections.defaultdict(list)
    for (a, b), c in pairs.items():
        by_first[a].append((c, b))

    kept = 0
    with open(path, "w", encoding="utf-8") as f:
        f.write("# hyprpad-osk prediction word list — generated by "
                "osk/tools/build-model/build-model.py\n")
        f.write("# %d words; unigram probabilities from wordfreq large_en, "
                "successors S(next|prev) from the bigram corpus.\n" % len(vocab))
        f.write("# See DATA-LICENSES.md for the sources and their licences.\n")
        for word, p in sorted(vocab.items(), key=lambda kv: (-kv[1], kv[0])):
            f.write("%s %.10g\n" % (word, min(max(p, 1e-15), 1.0)))
            succ = by_first.get(word)
            if not succ:
                continue
            total = totals.get(word, 0)
            if total <= 0:
                continue
            succ.sort(key=lambda t: (-t[0], t[1]))
            for count, nxt in succ[:max_successors]:
                s = count / total
                if s <= 0:
                    continue
                f.write("  %s %.10g\n" % (nxt, min(s, 1.0)))
                kept += 1
    log("wrote %s: %d words, %d successors" % (path, len(vocab), kept))


def log(msg):
    print("build-model: %s" % msg, file=sys.stderr, flush=True)


def die(msg):
    log("error: %s" % msg)
    raise SystemExit(1)


if __name__ == "__main__":
    raise SystemExit(main())
