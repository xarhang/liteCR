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
NAV = (
    ("Get started", (("Introduction", "docs/README.md"), ("Local setup", "docs/BETA-SETUP.md"),
                     ("Using liteCR", "docs/USAGE.md"), ("Beta limits", "docs/BETA.md"))),
    ("Understand & integrate", (("Architecture", "docs/ARCHITECTURE.md"), ("API reference", "docs/API.md"),
                                ("Access & permissions", "docs/contracts/ACCESS.md"),
                                ("FHIR R4 adapter", "docs/integration/FHIR-R4-BASELINE.md"),
                                ("Migration packages", "docs/migration/V1-PACKAGE.md"))),
    ("Operate", (("Deployment", "docs/DEPLOYMENT.md"), ("Installation", "tools/install/INSTALL.md"),
                 ("Database roles", "docs/runbooks/database-roles.md"),
                 ("Backup & restore", "docs/runbooks/backup-restore.md"),
                 ("Upgrade & rollback", "docs/runbooks/upgrade-rollback.md"))),
    ("Project", (("Contributing", "CONTRIBUTING.md"), ("Security", "SECURITY.md"),
                 ("License", "LICENSING.md"), ("Support", "SUPPORT.md"))),
)
REPOSITORY = "https://github.com/xarhang/liteCR"
MARK = '<svg class="brand-icon" viewBox="0 0 32 32" fill="none" aria-hidden="true"><rect x="1" y="1" width="30" height="30" rx="8" fill="currentColor"/><path d="M9 9v14h14M14 9v9h9" stroke="var(--color-on-action)" stroke-width="2.3" stroke-linejoin="round"/></svg>'


def header(prefix: str, home: bool) -> str:
    return f'''<header class="site-header"><div class="header-inner">
<a class="brand" href="{prefix}index.html" aria-label="liteCR home">{MARK}<span>lite<span class="brand-cr">CR</span></span></a>
<span class="version">Beta 0.1.2</span><nav class="top-nav" aria-label="Main navigation">
<a href="{prefix}index.html" {'aria-current="page"' if home else ''}>Overview</a>
<a href="{prefix}docs/README.html" {'' if home else 'aria-current="page"'}>Documentation</a>
<a href="{REPOSITORY}">GitHub <span aria-hidden="true">↗</span></a></nav></div></header>'''


def sidebar(prefix: str, current: str) -> str:
    groups = []
    for label, entries in NAV:
        links = ''.join(f'<a href="{prefix}{DEST[p]}"' + (' aria-current="page"' if p == current else '') + f'>{text}</a>' for text, p in entries)
        groups.append(f'<section><h2>{label}</h2>{links}</section>')
    return '<nav aria-label="Documentation guides">' + ''.join(groups) + '</nav>'


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
        headings = []
        for index, token in enumerate(tokens):
            if token.type == "heading_open":
                slug = re.sub(r"[^\w-]+", "-", tokens[index + 1].content.lower()).strip("-") or "section"
                used_ids[slug] = used_ids.get(slug, 0) + 1
                token.attrSet("id", slug if used_ids[slug] == 1 else f"{slug}-{used_ids[slug]}")
                if token.tag == 'h2':
                    headings.append((token.attrGet('id'), tokens[index + 1].content))
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
        home = name == 'README.md'
        if home:
            title = 'Patient identity registry and documentation'
            content = '<main id="content" class="home" tabindex="-1">' + (ROOT / 'tools/docs/home.html').read_text() + '</main>'
        else:
            toc = ''.join(f'<a href="#{html.escape(anchor)}">{html.escape(label)}</a>' for anchor, label in headings)
            content = f'''<div class="docs-layout">
<aside class="docs-sidebar"><div class="sidebar-desktop">{sidebar(prefix, name)}</div><details class="sidebar-mobile"><summary>Browse documentation</summary>{sidebar(prefix, name)}</details></aside>
<main id="content" class="doc-main" tabindex="-1"><div class="breadcrumb"><a href="{prefix}index.html">Home</a><span aria-hidden="true">/</span><a href="{prefix}docs/README.html">Docs</a></div>
<article class="prose">{md.renderer.render(tokens, md.options, {})}</article>
<div class="doc-bottom"><a href="{REPOSITORY}/blob/main/{name}">View this guide on GitHub ↗</a><a href="{prefix}SUPPORT.html">Get help →</a></div></main>
<aside class="page-toc"><nav aria-label="On this page"><h2>On this page</h2>{toc}<a class="toc-top" href="#content">Back to top ↑</a></nav></aside></div>'''
        document = f'''<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'self'; img-src 'self'; base-uri 'none'; form-action 'none'">
<meta name="description" content="liteCR patient identity registry. Learn how to set up, evaluate, integrate and operate the experimental Beta with synthetic data.">
<title>{html.escape(title)} · liteCR</title><link rel="stylesheet" href="{prefix}assets/site.css"></head>
<body><a class="skip" href="#content">Skip to content</a>
{header(prefix, home)}{content}
<footer class="site-footer"><div><a class="brand" href="{prefix}index.html">{MARK}<span>liteCR</span></a><p>Patient identity. Source context. Human review.</p></div>
<nav aria-label="Footer"><a href="{prefix}LICENSING.html">AGPLv3+</a><a href="{prefix}SECURITY.html">Security</a><a href="{prefix}CONTRIBUTING.html">Contribute</a><a href="{prefix}docs/BETA.html">Beta limits</a></nav>
<p class="footer-note">Experimental software · Evaluate with synthetic data. This website is documentation, not a running registry.</p></footer>
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
    shutil.copyfile(ROOT / "packages/design-system/src/tokens.css", output / "assets/tokens.css")
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
