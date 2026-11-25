# Pith -> github-linguist PR checklist

## 1) Fork and branch

```bash
git clone https://github.com/<your-user>/linguist.git
cd linguist
git remote add upstream https://github.com/github-linguist/linguist.git
git fetch upstream
git checkout -b pith-language upstream/main
```

## 2) Add Pith language metadata

- Update `lib/linguist/languages.yml` with the entry from `languages.yml.snippet.yml`

## 3) Add grammar

- Add `grammars/pith.tmLanguage.json` (or the exact grammar location expected by Linguist)
- Ensure the grammar scope is `source.pith`

## 4) Add samples

- Add representative files under `samples/Pith/`
- Include declarations, interpolation, generics, match, impl/interface, and indentation blocks

## 5) Run local checks in Linguist

- Run the Linguist test suite commands the repo expects
- Verify that `.pith` resolves to Pith in local Linguist output

## 6) Open PR

Include:
- A concise language rationale
- A link to the Pith repo and language docs
- A note that `.pith` is currently mapped as a temporary workaround in the Pith repo
