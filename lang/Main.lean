import Omar.Compiler

open Omar

def usage : String :=
  "usage: omarc [--local-imports] <input.omar> <output.json>\n       \
   omarc [--local-imports] --diff <old.omar> <new.omar> <diff.json>"

/-- Whether an import path stays inside the program's own directory: relative,
    and never climbing out. What a program staged from a request may import;
    one the operator points the compiler at may import anything they can read. -/
def isLocalImport (path : String) : Bool :=
  let file := System.FilePath.mk path
  !file.isAbsolute && !path.startsWith "/" && !path.startsWith "\\" &&
    !(path.splitOn "\\").length > 1 && !file.components.contains ".."

/-- A program's source and the schemas it imports, read from beside it. -/
def readProgram (localImports : Bool) (input : String) :
    IO (String × String × Array (String × String)) := do
  let source ← IO.FS.readFile input
  -- The program takes its source file's name, the way a C binary takes
  -- its file's name rather than `main`'s.
  let programName := (System.FilePath.mk input).fileStem.getD "main"
  let imports ← match schemaImports source with
    | .ok imports => pure imports
    | .error message => throw (IO.userError s!"{input}: {message}")
  let schemas ← imports.mapM fun (name, path) => do
    if localImports && !isLocalImport path then
      throw (IO.userError s!"schema type '{name}' from '{path}': \
        an import must be a relative path inside the program's directory")
    let schemaPath := (System.FilePath.mk input).parent.getD (System.FilePath.mk ".") /
      System.FilePath.mk path
    try
      pure (path, ← IO.FS.readFile schemaPath)
    catch error =>
      throw (IO.userError s!"schema type '{name}' from '{schemaPath}': {error}")
  pure (programName, source, schemas)

def main (args : List String) : IO UInt32 := do
  let (localImports, args) := match args with
    | "--local-imports" :: rest => (true, rest)
    | args => (false, args)
  -- With `--diff`, the output is the plan that turns the old program into
  -- the new one, rather than a program's own bytecode.
  let job ← match args with
    | ["--diff", old, new, output] => pure (some (some old, new, output))
    | [input, output] => pure (some (none, input, output))
    | _ => pure none
  let some (old, input, output) := job | do
    IO.eprintln usage
    return 2
  try
    let (programName, source, schemas) ← readProgram localImports input
    let compiled ← match old with
      | none => pure (compileSourceWithSchemas programName source schemas)
      | some old => do
        let (oldName, oldSource, oldSchemas) ← readProgram localImports old
        pure (compileDiffSourcesWithSchemas oldName oldSource oldSchemas
          programName source schemas)
    match compiled with
    | .ok bytecode =>
        IO.FS.writeFile output bytecode
        IO.println s!"compiled {input} -> {output}"
        pure 0
    | .error message =>
        IO.eprintln s!"{input}: {message}"
        pure 1
  catch error =>
    IO.eprintln error.toString
    pure 1
