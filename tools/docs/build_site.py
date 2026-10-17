"""Build an explicitly allowlisted, script-free documentation site."""

from __future__ import annotations

import argparse
import html
from html.parser import HTMLParser
from pathlib import Path
import re
import shutil
from urllib.parse import unquote, urlsplit

from markdown_it import MarkdownIt

ROOT = Path(__file__).resolve().parents[2]
PAGES = (
    "README.md", "CONTRIBUTING.md", "SECURITY.md", "LICENSING.md", "CODE_OF_CONDUCT.md", "SUPPORT.md",
    "docs/README.md", "docs/BETA.md", "docs/BETA-SETUP.md", "docs/USAGE.md",
    "docs/API.md", "docs/ARCHITECTURE.md", "docs/THEMES.md", "docs/DEPLOYMENT.md",
    "docs/GITHUB-PAGES.md", "docs/contracts/ACCESS.md", "docs/contracts/fhir-r4-profile.md",
    "docs/integration/FHIR-R4-BASELINE.md", "docs/migration/V1-PACKAGE.md",
    "docs/adr/001-rust-backend.md", "docs/adr/002-first-party-postgres-worker.md",
    "docs/runbooks/backup-restore.md", "docs/runbooks/database-roles.md",
    "docs/runbooks/upgrade-rollback.md", "docs/runbooks/migration-operator.md",
    "docs/runbooks/redacted-diagnostics.md", "tools/install/INSTALL.md",
    "infra/dev/README.md", "infra/dev/keycloak/README.md",
)
DEST = {name: ("index.html" if name == "README.md" else name[:-3] + ".html") for name in PAGES}
NAV = (("Overview", "README.md"), ("Setup", "docs/BETA-SETUP.md"),
       ("Usage", "docs/USAGE.md"), ("API", "docs/API.md"),
       ("All guides", "docs/README.md"), ("Security", "SECURITY.md"),
       ("Contribute", "CONTRIBUTING.md"), ("License", "LICENSING.md"))


def build(output: Path) -> None:
    if output.exists() and any(output.iterdir()):
        raise ValueError("Choose a new or empty output directory")
    output.mkdir(parents=True, exist_ok=True)
    md = MarkdownIt("commonmark", {"html": False}).enable("table")
    attachments: dict[str, Path] = {}
    for name, destination in DEST.items():
        depth = len(Path(destination).parts) - 1
        prefix = "../" * depth
        tokens = md.parse((ROOT / name).read_text(encoding="utf-8-sig"))
        used_ids: dict[str, int] = {}
        for index, token in enumerate(tokens):
            if token.type == "heading_open":
                slug = re.sub(r"[^\w-]+", "-", tokens[index + 1].content.lower()).strip("-") or "section"
                used_ids[slug] = used_ids.get(slug, 0) + 1
                token.attrSet("id", slug if used_ids[slug] == 1 else f"{slug}-{used_ids[slug]}")
            for child in token.children or []:
                if child.type != "link_open":
                    continue
                href = child.attrGet("href") or ""
                url = urlsplit(href)
                if url.scheme or url.netloc or not url.path:
                    continue
                source = (ROOT / name).parent.joinpath(unquote(url.path)).resolve()
                relative = source.relative_to(ROOT).as_posix()
                if relative in DEST:
                    target = DEST[relative]
                else:
                    allowed = source.suffix in {".rs", ".sql", ".sh", ".json", ".toml"} or relative == "LICENSE"
                    if not allowed or not source.is_file() or source.stat().st_size > 1_048_576:
                        raise ValueError(f"Unpublished reference in {name}: {relative}")
                    target = "source/" + relative + ".txt"
                    attachments[target] = source
                child.attrSet("href", prefix + target + ("#" + url.fragment if url.fragment and relative in DEST else ""))
        title = next((tokens[i + 1].content for i, t in enumerate(tokens) if t.type == "heading_open"), "liteCR")
        links = "".join(f'<a href="{prefix}{DEST[p]}"' + (' aria-current="page"' if p == name else '') + f'>{label}</a>' for label, p in NAV)
        document = f'''<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'self'; img-src 'self'; base-uri 'none'; form-action 'none'">
<title>{html.escape(title)} · liteCR</title><link rel="stylesheet" href="{prefix}assets/site.css"></head>
<body><a class="skip" href="#content">Skip to content</a>
<header><a class="brand" href="{prefix}index.html">liteCR <span>Documentation</span></a>
<p class="notice">Experimental Beta · Synthetic data only</p></header>
<div class="layout"><nav aria-label="Documentation">{links}</nav>
<main id="content" tabindex="-1">{md.renderer.render(tokens, md.options, {})}</main></div>
<footer>AGPL-3.0-or-later · <a href="{prefix}{DEST['docs/BETA.md']}">Beta limits</a> · Static documentation; run the server for registry workflows.</footer>
</body></html>'''
        path = output / destination
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(document, encoding="utf-8")
    for target, source in attachments.items():
        path = output / target
        path.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, path)
    (output / "assets").mkdir()
    shutil.copyfile(ROOT / "tools/docs/site.css", output / "assets/site.css")
    (output / ".nojekyll").write_text("")
    validate(output)
    print(f"Built and link-checked {len(PAGES)} pages and {len(attachments)} source references")


def validate(output: Path) -> None:
    class Links(HTMLParser):
        def __init__(self) -> None:
            super().__init__()
            self.links: list[str] = []
            self.ids: set[str] = set()

        def handle_starttag(self, tag, attrs):
            attrs = dict(attrs)
            if "id" in attrs:
                self.ids.add(attrs["id"])
            if tag in {"a", "link"} and "href" in attrs:
                self.links.append(attrs["href"])

    documents = {}
    for path in output.rglob("*.html"):
        parser = Links()
        parser.feed(path.read_text())
        documents[path.resolve()] = parser
    for path, parser in documents.items():
        for href in parser.links:
            url = urlsplit(href)
            if url.scheme or url.netloc:
                continue
            target = (path.parent / unquote(url.path)).resolve() if url.path else path
            target.relative_to(output.resolve())
            if not target.is_file():
                raise ValueError(f"Broken site link: {href} in {path}")
            if url.fragment and (target not in documents or unquote(url.fragment) not in documents[target].ids):
                raise ValueError(f"Broken site anchor: {href} in {path}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    build(parser.parse_args().output.resolve())
