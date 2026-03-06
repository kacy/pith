# forge syntax highlighting

this folder is the source of truth for forge syntax highlighting assets.

## contents

- `forge.tmLanguage.json`: TextMate grammar for `.fg` files
- `samples/*.fg`: representative forge snippets used to keep grammar coverage stable
- `validate.sh`: non-destructive checks for grammar json validity and baseline scope/sample coverage
- `linguist/`: files and notes to prepare the upstream github-linguist pull request

## local validation

```bash
./tooling/highlighting/validate.sh
```

## temporary github workaround

the repo currently forces `.fg` to python highlighting through `.gitattributes`.
this is temporary and will be removed once forge lands in github linguist.
