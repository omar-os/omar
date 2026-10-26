import Omar.Compiler

open Lean
open Omar

def usage : String :=
  "usage: omarc [--local-imports] [--mutate-from <running.json>] <input.omar> <output.json>"

/-- Whether an import path stays inside the program's own directory: relative,
    and never climbing out. What a program staged from a request may import;
    one the operator points the compiler at may import anything they can read. -/
def isLocalImport (path : String) : Bool :=
  let file := System.FilePath.mk path
  !file.isAbsolute && !path.startsWith "/" && !path.startsWith "\\" &&
    !(path.splitOn "\\").length > 1 && !file.components.contains ".."

def main (args : List String) : IO UInt32 := do
  let (localImports, args) := match args with
    | "--local-imports" :: rest => (true, rest)
    | args => (false, args)
  -- A running program's bytecode: the output is then the plan that turns it
  -- into the input, rather than the input's own bytecode.
  let (running, args) := match args with
    | "--mutate-from" :: path :: rest => (some path, rest)
    | args => (none, args)
  match args with
  | [input, output] =>
      try
        let source ← IO.FS.readFile input
        -- The program takes its source file's name, the way a C binary takes
        -- its file's name rather than `main`'s.
        let programName := (System.FilePath.mk input).fileStem.getD "main"
        let imports ← match schemaImports source with
          | .ok imports => pure imports
          | .error message => throw (IO.userError message)
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
        let compiled ← match running with
          | none => pure (compileSourceWithSchemas programName source schemas)
          | some path => do
            let running ← match Json.parse (← IO.FS.readFile path) with
              | .ok running => pure running
              | .error message => throw (IO.userError s!"{path}: {message}")
            pure (compileMutationSourceWithSchemas running programName source schemas)
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
  | _ =>
      IO.eprintln usage
      pure 2
