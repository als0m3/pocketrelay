"""Convert PDFs into text and page images for backends without native PDF support.

Extract text per page; render pages with little text (scans and
diagrams) as PNG so the model can read them visually.
"""

import base64
import io
import os
import threading

MAX_IMAGE_PAGES = int(os.environ.get("REMOTE_PDF_MAX_IMAGE_PAGES", "20"))
MAX_TEXT_CHARS = int(os.environ.get("REMOTE_PDF_MAX_TEXT_CHARS", "400000"))
# auto: render sparse pages; images: render all pages; text: never render images.
MODE = os.environ.get("REMOTE_PDF_MODE", "auto")
SPARSE_CHARS = 40  # Treat pages below this threshold as scanned images.
RENDER_SCALE = 2.0  # Approximately 144 dpi for A4 pages.
MAX_SIDE = 2048     # Larger images are downscaled by the model, reducing OCR quality.

_lock = threading.Lock()  # pdfium is not thread-safe.


class PdfError(Exception):
    pass


def convert(data_b64: str, title: str | None = None) -> list[dict]:
    """Return Anthropic blocks: one text block followed by image blocks."""
    import pypdfium2 as pdfium

    raw = base64.b64decode(data_b64)
    with _lock:
        try:
            doc = pdfium.PdfDocument(raw)
        except pdfium.PdfiumError as e:
            raise PdfError(f"PDF illisible : {e}") from e
        try:
            n = len(doc)
            texts, to_render = [], []
            for i in range(n):
                page = doc[i]
                tp = page.get_textpage()
                txt = tp.get_text_range().replace("\r\n", "\n").strip()
                tp.close()
                texts.append(txt)
                if MODE == "images" or (MODE == "auto" and len(txt) < SPARSE_CHARS):
                    if len(to_render) < MAX_IMAGE_PAGES:
                        to_render.append(i)
                page.close()
            images = []
            for i in to_render:
                page = doc[i]
                w, h = page.get_size()
                pil = page.render(scale=min(RENDER_SCALE, MAX_SIDE / max(w, h))).to_pil()
                page.close()
                buf = io.BytesIO()
                pil.convert("RGB").save(buf, format="PNG", optimize=True)
                images.append((i, base64.b64encode(buf.getvalue()).decode()))
        finally:
            doc.close()

    name = f' name="{title}"' if title else ""
    parts = [f"<document{name} pages=\"{n}\">"]
    total = 0
    for i, txt in enumerate(texts):
        body = txt if txt else ("[page without text: see attached image]" if i in to_render else "[page without text]")
        if total + len(body) > MAX_TEXT_CHARS:
            parts.append(f"[… text truncated after page {i} of {n}]")
            break
        total += len(body)
        parts.append(f"--- page {i + 1} ---\n{body}")
    skipped = [i for i, t in enumerate(texts) if len(t) < SPARSE_CHARS and i not in to_render and MODE != "text"]
    if skipped:
        parts.append(f"[{len(skipped)} scanned page(s) omitted: limit of {MAX_IMAGE_PAGES} images]")
    parts.append("</document>")
    blocks = [{"type": "text", "text": "\n".join(parts) + "\n"}]
    for i, png in images:
        blocks.append({"type": "text", "text": f"[image of page {i + 1}{' of ' + title if title else ''}]\n"})
        blocks.append({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": png},
                       "detail": "high"})  # Read only by the Codex backend.
    return blocks
