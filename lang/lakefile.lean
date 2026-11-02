import Lake
open Lake DSL

package omarLang

-- Every module, not only those `omarc` imports: `Omar.Verify` is never linked
-- into it but is imported by each property file it checks.
@[default_target]
lean_lib Omar where
  globs := #[.submodules `Omar]

@[default_target]
lean_exe omarc where
  root := `Main

@[test_driver]
lean_exe omarLangTests where
  root := `Tests
