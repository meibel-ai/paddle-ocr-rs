"""Costruisce il corpus di benchmark OCR da PDF nativi digitali (Fase 6 di PLAN.md).

Approccio deciso dall'autore: si parte da PDF **nativi digitali**, la ground
truth è il testo nativo estratto dallo stesso PDF (via pypdfium2, lo stesso
motore del progetto — gratis e allineata), le pagine vengono rasterizzate a
200 DPI e poi degradate con **Augraphy** a livelli crescenti. Così il
benchmark misura non solo "quante parole vere" ma *a che livello di degrado*
ogni motore cede.

Uso:
    python tools/benchmark/make_benchmark.py benchmark/src/*.pdf
    python tools/benchmark/make_benchmark.py --out benchmark/out doc.pdf

Output, per ogni documento e pagina:
    benchmark/out/<doc>/page-NNN/
        L0.png         raster pulito a 200 DPI (baseline: l'OCR qui deve ~100%)
        L1.png         degrado lieve: rotazione 0,3-1°, JPEG, rumore sottile
        L2.png         degrado medio: fotocopia cattiva, illuminazione, ink bleed
        L3.png         degrado forte: fax (halftone+binarizzazione), pieghe
        verita.txt     testo nativo della pagina (ground truth)

Determinismo: il seed è derivato da (SEED, documento, pagina, livello) — due
esecuzioni producono gli stessi pixel; rigenerare un solo livello è sicuro.

Limite dichiarato (v. PLAN.md): il rumore sintetico non copre i difetti di
scansione fisica (pieghe reali, timbri, mezzitoni della carta) — da integrare
più avanti con scansioni vere.
"""

from __future__ import annotations

import argparse
import random
import sys
from pathlib import Path

import cv2
import numpy as np
import pypdfium2 as pdfium

# Stesso tetto del ramo OCR del progetto (misura di edito-ocr-v6: sopra i
# 200 DPI il recognizer non guadagna, e sulle pagine vettoriali peggiora).
DPI = 200
SEED = 20260823


def _seed_for(doc: str, page: int, level: int) -> int:
    return abs(hash((SEED, doc, page, level))) % (2**32)


def _build_pipeline(level: int):
    """Le pipeline Augraphy per livello. Import locale: costruttori leggeri,
    ma l'import di augraphy è lento e serve solo qui."""
    from augraphy import (
        AugraphyPipeline, BadPhotoCopy, BrightnessTexturize, Faxify, Folding,
        Geometric, InkBleed, Jpeg, LightingGradient, NoiseTexturize, SubtleNoise,
    )

    if level == 1:
        # Lieve: quello che fa una buona scansione da ufficio.
        return AugraphyPipeline(
            ink_phase=[],
            paper_phase=[SubtleNoise(subtle_range=8)],
            post_phase=[Jpeg(quality_range=(70, 85))],
        )
    if level == 2:
        # Medio: fotocopia stanca, luce non uniforme, inchiostro che sbava.
        return AugraphyPipeline(
            ink_phase=[InkBleed(intensity_range=(0.2, 0.4), kernel_size=(3, 3))],
            paper_phase=[
                BrightnessTexturize(texturize_range=(0.85, 0.99)),
                NoiseTexturize(sigma_range=(2, 5), turbulence_range=(2, 3)),
            ],
            post_phase=[
                LightingGradient(max_brightness=255, min_brightness=196),
                Jpeg(quality_range=(50, 70)),
            ],
        )
    if level == 3:
        # Forte: fax con halftone e binarizzazione, pieghe, rumore da copia.
        return AugraphyPipeline(
            ink_phase=[],
            paper_phase=[],
            post_phase=[
                Folding(fold_count=2, fold_noise=0.05),
                BadPhotoCopy(noise_type=1, noise_iteration=(1, 1),
                             noise_sparsity=(0.3, 0.6),
                             noise_concentration=(0.1, 0.3)),
                Faxify(monochrome=1, halftone=1, invert=1,
                       half_kernel_size=(1, 1), angle=(0, 360), sigma=(1, 2)),
            ],
        )
    raise ValueError(f"livello sconosciuto: {level}")


def _rotate_slight(img: np.ndarray, rng: random.Random) -> np.ndarray:
    """Rotazione fine 0,3-1° (segno casuale), bordo replicato.

    Fatta con OpenCV e non con Geometric di Augraphy per controllare il range
    sub-grado esatto: è il caso che mette in crisi i word box CTC
    (recognize.py di v6 misurava box 1,34× più alti già a 0,56°).
    """
    angle = rng.uniform(0.3, 1.0) * rng.choice((-1, 1))
    h, w = img.shape[:2]
    m = cv2.getRotationMatrix2D((w / 2, h / 2), angle, 1.0)
    return cv2.warpAffine(img, m, (w, h), flags=cv2.INTER_LINEAR,
                          borderMode=cv2.BORDER_REPLICATE)


def degrade(img: np.ndarray, level: int, doc: str, page: int) -> np.ndarray:
    if level == 0:
        return img
    rng = random.Random(_seed_for(doc, page, level))
    random.seed(rng.random())
    np.random.seed(_seed_for(doc, page, level))
    out = _rotate_slight(img, rng)
    out = _build_pipeline(level)(out)
    # Alcune augmentation restituiscono grayscale: uniformare a BGR.
    if out.ndim == 2:
        out = cv2.cvtColor(out, cv2.COLOR_GRAY2BGR)
    return out


def process_pdf(pdf_path: Path, out_root: Path, levels: list[int]) -> int:
    doc_name = pdf_path.stem
    doc = pdfium.PdfDocument(str(pdf_path))
    n_pages = len(doc)
    for i in range(n_pages):
        # Una pagina malformata non deve abortire il documento: si salta e lo
        # si dice (regola del progetto: nessuno scarto silenzioso).
        try:
            page = doc[i]
        except Exception as exc:
            print(f"  ⚠ {doc_name} pagina {i + 1}: illeggibile, saltata ({exc})",
                  file=sys.stderr)
            continue
        page_dir = out_root / doc_name / f"page-{i + 1:03d}"
        page_dir.mkdir(parents=True, exist_ok=True)

        # Ground truth: testo nativo, in "reading order" di pdfium.
        text = page.get_textpage().get_text_range()
        (page_dir / "verita.txt").write_text(text, encoding="utf-8")

        # Raster a 200 DPI.
        bitmap = page.render(scale=DPI / 72)
        pil = bitmap.to_pil().convert("RGB")
        img = cv2.cvtColor(np.array(pil), cv2.COLOR_RGB2BGR)

        for level in levels:
            target = page_dir / f"L{level}.png"
            if target.exists():
                continue
            cv2.imwrite(str(target), degrade(img, level, doc_name, i))
        print(f"  {doc_name} pagina {i + 1}/{n_pages}: "
              f"{len(text.split())} parole di verità, livelli {levels}")
    return n_pages


def main() -> int:
    # La console Windows non è sempre UTF-8: meglio sostituire che morire.
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("pdf", nargs="+", type=Path, help="PDF nativi digitali")
    ap.add_argument("--out", type=Path, default=Path("benchmark/out"))
    ap.add_argument("--levels", default="0,1,2,3",
                    help="livelli di degrado da generare (default 0,1,2,3)")
    args = ap.parse_args()

    levels = [int(x) for x in args.levels.split(",")]
    total = 0
    for pdf in args.pdf:
        if not pdf.exists():
            print(f"non trovato: {pdf}", file=sys.stderr)
            return 1
        print(f"{pdf}:")
        total += process_pdf(pdf, args.out, levels)
    print(f"fatto: {total} pagine in {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
