# Pith syntax highlighting

This folder is the source of truth for Pith syntax highlighting assets.

## Contents

- `pith.tmLanguage.json`: TextMate grammar for `.pith` files
- `samples/*.pith`: representative Pith snippets used to keep grammar coverage stable
- `validate.sh`: non-destructive checks for grammar JSON validity and baseline scope and sample coverage
- `linguist/`: files and notes to prepare the upstream GitHub Linguist pull request

## Local validation

```bash
./tooling/highlighting/validate.sh
```

## Temporary GitHub workaround

The repo currently forces `.pith` to Python highlighting through `.gitattributes`.
This is temporary and will be removed once Pith lands in GitHub Linguist.
