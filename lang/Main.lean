import Omar.Compiler

open Omar

def usage : String := "usage: omarc <input.omar> <output.json>"

def main (args : List String) : IO UInt32 := do
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
          let schemaPath := (System.FilePath.mk input).parent.getD (System.FilePath.mk ".") /
            System.FilePath.mk path
          try
            pure (path, ← IO.FS.readFile schemaPath)
          catch error =>
            throw (IO.userError s!"schema type '{name}' from '{schemaPath}': {error}")
        match compileSourceWithSchemas programName source schemas with
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
