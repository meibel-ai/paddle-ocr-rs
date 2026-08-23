"""Scoring del benchmark OCR: confronto a multiset di parole (Fase 6 di PLAN.md).

Il confronto è a **multiset di parole per pagina**, non a stringa concatenata
(lezione di edito-ocr-v6 `probe.py:126-132`: due colonne lette in ordine
diverso non sono contenuto diverso). Metriche: precision / recall / F1 sulle
parole, dove una parola in più nell'ipotesi costa precision e una persa costa
recall.

Uso:
    python tools/benchmark/score.py verita.txt ipotesi.txt
    python tools/benchmark/score.py --dir benchmark/out/doc --hyp-suffix .L2.ocr.txt
    (modo --dir: per ogni page-NNN confronta verita.txt con page-NNN/<suffix>)

Normalizzazione volutamente minima: lowercase + trim della punteggiatura ai
bordi. Niente correzioni "generose" (accenti, ligature): se l'OCR le sbaglia,
deve costare.
"""

from __future__ import annotations

import argparse
import json
import sys
import unicodedata
from collections import Counter
from pathlib import Path

# Punteggiatura da spogliare ai bordi parola. L'interna (l'apostrofo di
# "dell'atto", il punto di "S.p.A.") resta: fa parte della parola.
_STRIP = "".join(
    chr(c) for c in range(0x2000) if unicodedata.category(chr(c)).startswith("P")
)


def words(text: str) -> Counter:
    out: Counter = Counter()
    for tok in text.split():
        tok = tok.strip(_STRIP).lower()
        if tok:
            out[tok] += 1
    return out


def score(truth: str, hyp: str) -> dict:
    t, h = words(truth), words(hyp)
    hit = sum((t & h).values())
    n_truth, n_hyp = sum(t.values()), sum(h.values())
    precision = hit / n_hyp if n_hyp else 0.0
    recall = hit / n_truth if n_truth else 1.0
    f1 = 2 * precision * recall / (precision + recall) if precision + recall else 0.0
    return {
        "parole_verita": n_truth,
        "parole_ipotesi": n_hyp,
        "corrette": hit,
        "precision": round(precision, 4),
        "recall": round(recall, 4),
        "f1": round(f1, 4),
        # le più frequenti tra perse e inventate, per il debug a occhio
        "perse": [w for w, _ in (t - h).most_common(10)],
        "inventate": [w for w, _ in (h - t).most_common(10)],
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("files", nargs="*", type=Path,
                    help="verita.txt ipotesi.txt (modo singolo)")
    ap.add_argument("--dir", type=Path,
                    help="cartella documento del benchmark (page-NNN/…)")
    ap.add_argument("--hyp-suffix", default=".ocr.txt",
                    help="nome file ipotesi dentro ogni page-NNN")
    ap.add_argument("--json", action="store_true", help="output JSON completo")
    args = ap.parse_args()

    if args.dir:
        rows = []
        for page_dir in sorted(args.dir.glob("page-*")):
            truth_f = page_dir / "verita.txt"
            hyp_f = page_dir / args.hyp_suffix.lstrip(".") \
                if not args.hyp_suffix.startswith(".") \
                else page_dir / args.hyp_suffix[1:]
            if not hyp_f.exists():
                print(f"{page_dir.name}: ipotesi mancante ({hyp_f.name})",
                      file=sys.stderr)
                continue
            s = score(truth_f.read_text(encoding="utf-8"),
                      hyp_f.read_text(encoding="utf-8"))
            s["pagina"] = page_dir.name
            rows.append(s)
        if not rows:
            print("nessuna pagina con ipotesi", file=sys.stderr)
            return 1
        if args.json:
            print(json.dumps(rows, ensure_ascii=False, indent=1))
        else:
            for s in rows:
                print(f"{s['pagina']}: F1 {s['f1']:.3f} "
                      f"(P {s['precision']:.3f} / R {s['recall']:.3f}, "
                      f"{s['corrette']}/{s['parole_verita']} parole)")
            f1s = [s["f1"] for s in rows]
            print(f"media F1: {sum(f1s) / len(f1s):.4f} su {len(rows)} pagine")
        return 0

    if len(args.files) != 2:
        ap.error("servono verita.txt e ipotesi.txt, oppure --dir")
    s = score(args.files[0].read_text(encoding="utf-8"),
              args.files[1].read_text(encoding="utf-8"))
    print(json.dumps(s, ensure_ascii=False, indent=1) if args.json else
          f"F1 {s['f1']:.4f}  P {s['precision']:.4f}  R {s['recall']:.4f}  "
          f"({s['corrette']}/{s['parole_verita']})\n"
          f"perse: {' '.join(s['perse'])}\ninventate: {' '.join(s['inventate'])}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
