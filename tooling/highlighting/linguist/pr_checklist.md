# pith -> github-linguist PR checklist

## 1) fork and branch

```bash
git clone https://github.com/<your-user>/linguist.git
cd linguist
git remote add upstream https://github.com/github-linguist/linguist.git
git fetch upstream
git checkout -b pith-language upstream/main
```

## 2) add pith language metadata

- update `lib/linguist/languages.yml` with the entry from `languages.yml.snippet.yml`

## 3) add grammar

- add `grammars/pith.tmLanguage.json` (or the exact grammar location expected by linguist)
- ensure grammar scope is `source.pith`

## 4) add samples

- add representative files under `samples/Pith/`
- include declarations, interpolation, generics, match, impl/interface, and indentation blocks

## 5) run local checks in linguist

- run linguist test suite commands expected by the repo
- verify `.pith` resolves to pith in local linguist output

## 6) open PR

include:
- concise language rationale
- link to pith repo and language docs
- note that `.pith` is currently mapped as a temporary workaround in the pith repo
