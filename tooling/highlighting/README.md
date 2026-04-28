# pith syntax highlighting

this folder is the source of truth for pith syntax highlighting assets.

## contents

- `pith.tmLanguage.json`: TextMate grammar for `.pith` files
- `samples/*.pith`: representative pith snippets used to keep grammar coverage stable
- `validate.sh`: non-destructive checks for grammar json validity and baseline scope/sample coverage
- `linguist/`: files and notes to prepare the upstream github-linguist pull request

## local validation

```bash
./tooling/highlighting/validate.sh
```

## temporary github workaround

the repo currently forces `.pith` to python highlighting through `.gitattributes`.
this is temporary and will be removed once pith lands in github linguist.
