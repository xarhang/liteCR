# Documentation website

The website has a product overview and renders the maintained public Markdown guides, including setup, usage, API, security and contribution guidance. It works under a project subpath such as `/liteCR/` and uses local styles with no browser scripts, analytics, remote fonts or external runtime dependencies. Documentation pages include grouped navigation and an on-page table of contents; mobile navigation and FAQs use native disclosures.

GitHub Pages hosts static files. It cannot run the liteCR Rust API, PostgreSQL or an IdP. Follow [local setup](BETA-SETUP.md) to run the registry. The documentation site has no login or patient-data entry form.

## Local preview

Use Python 3.10 or newer in a virtual environment. Install the documentation dependencies, then choose a new output directory:

```sh
python -m venv .venv-docs
# Linux/macOS: . .venv-docs/bin/activate
# PowerShell: .venv-docs/Scripts/Activate.ps1
python -m pip install -r tools/docs/requirements.txt
python tools/docs/build_site.py --output target/docs-site
python -m http.server 8000 --bind 127.0.0.1 --directory target/docs-site
```

Open `http://127.0.0.1:8000`. The builder rejects an occupied output directory; use a new directory for another preview. Only explicitly listed public guides and their validated source references are copied. Internal reports, environment files, test runtime data and release archives are excluded.

## Publish on GitHub

1. Push the reviewed candidate to [xarhang/liteCR](https://github.com/xarhang/liteCR). The project-site address is `https://xarhang.github.io/liteCR/`.
2. In repository **Settings → Pages → Build and deployment**, select **GitHub Actions**.
3. A push to the default branch builds, HTTP-checks and deploys the documentation. You can also run **Documentation Pages** manually on the default branch with `publish` set to true. Pull requests and other branches build and validate without deployment.
4. Open the URL reported by the successful `github-pages` deployment. Verify the landing page, setup, usage, security links and mobile layout there. A local build is not proof of successful hosting.

The deployment job uses `pages: write` and `id-token: write` only for publication; build jobs use read-only repository access. Set environment protection and branch rules in the repository as needed; this workflow does not configure those settings.

See [GitHub's custom Pages workflow documentation](https://docs.github.com/en/pages/getting-started-with-github-pages/using-custom-workflows-with-github-pages).
