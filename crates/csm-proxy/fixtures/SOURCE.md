# Sample EPUB fixture

`crosspoint-reader.epub` is a generic EPUB 2 book built from the
CrossPoint Reader README on branch `develop`.

Source: https://github.com/canardleteer/crosspoint-reader/blob/develop/README.md

`crosspoint-reader.README.md` is the fetched markdown with the GitHub
badge and local cover image dropped so the book stays self-contained.

Regenerate:

```bash
curl -sS -o crates/csm-proxy/fixtures/crosspoint-reader.README.md \
  https://raw.githubusercontent.com/canardleteer/crosspoint-reader/develop/README.md
# drop the badge line and `docs/images/cover.jpg` embed, then:
pandoc crates/csm-proxy/fixtures/crosspoint-reader.README.md \
  --from markdown --to epub2 \
  --output crates/csm-proxy/fixtures/crosspoint-reader.epub \
  --metadata title="CrossPoint Reader" \
  --metadata author="CrossPoint contributors" \
  --metadata language="en" \
  --epub-title-page=false
```

`start_instance` copies this file to `fs_/books/CrossPoint-Reader.epub`
under the instance working directory when `sample_book` is true.
