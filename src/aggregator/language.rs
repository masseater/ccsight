//! Filename / extension / glob → programming-language classification.
//!
//! Used by `StatsAggregator` (to attribute Read/Edit/Write/Glob/Grep tool calls
//! to a language) and by the Languages panel UI (to decide whether an extension
//! is "known" or should be displayed as a raw `.xyz` row). Lives in its own
//! module so the 250 lines of static lookup tables don't drown out the actual
//! aggregation logic in `stats.rs`.

pub(crate) fn from_type_filter(type_filter: &str) -> Option<&'static str> {
    match type_filter {
        "ada" => Some("Ada"),
        "astro" => Some("Astro"),
        "c" | "h" => Some("C"),
        "clojure" | "clj" => Some("Clojure"),
        "cpp" | "c++" => Some("C++"),
        "cs" | "csharp" => Some("C#"),
        "css" | "scss" | "sass" | "less" => Some("CSS"),
        "d" => Some("D"),
        "dart" => Some("Dart"),
        "docker" | "dockerfile" => Some("Docker"),
        "elixir" => Some("Elixir"),
        "elm" => Some("Elm"),
        "erlang" | "erl" => Some("Erlang"),
        "fortran" => Some("Fortran"),
        "fs" | "fsharp" => Some("F#"),
        "gdscript" | "gd" => Some("GDScript"),
        "glsl" => Some("GLSL"),
        "go" => Some("Go"),
        "graphql" | "gql" => Some("GraphQL"),
        "haskell" | "hs" => Some("Haskell"),
        "html" => Some("HTML"),
        "java" => Some("Java"),
        "js" | "jsx" | "javascript" => Some("JavaScript"),
        "json" => Some("JSON"),
        "julia" | "jl" => Some("Julia"),
        "kotlin" | "kt" => Some("Kotlin"),
        "latex" | "tex" => Some("LaTeX"),
        "lua" => Some("Lua"),
        "md" | "markdown" => Some("Markdown"),
        "nim" => Some("Nim"),
        "nix" => Some("Nix"),
        "ocaml" | "ml" => Some("OCaml"),
        "perl" | "pl" => Some("Perl"),
        "php" => Some("PHP"),
        "powershell" | "ps1" => Some("PowerShell"),
        "proto" | "protobuf" => Some("Protobuf"),
        "purescript" | "purs" => Some("PureScript"),
        "py" | "python" => Some("Python"),
        "r" => Some("R"),
        "ruby" | "rb" => Some("Ruby"),
        "rust" => Some("Rust"),
        "scala" => Some("Scala"),
        "sh" | "shell" | "bash" | "zsh" | "fish" => Some("Shell"),
        "solidity" | "sol" => Some("Solidity"),
        "sql" => Some("SQL"),
        "svelte" => Some("Svelte"),
        "swift" => Some("Swift"),
        "terraform" | "tf" | "hcl" => Some("Terraform"),
        "toml" => Some("TOML"),
        "ts" | "tsx" | "typescript" => Some("TypeScript"),
        "vue" => Some("Vue"),
        "xml" => Some("XML"),
        "yaml" => Some("YAML"),
        "zig" => Some("Zig"),
        _ => None,
    }
}

pub fn for_extension(ext: &str) -> &'static str {
    match ext {
        // A
        "abap" => "ABAP",
        "ada" | "adb" | "ads" => "Ada",
        "apex" => "Apex",
        "applescript" | "scpt" => "AppleScript",
        "asm" | "s" | "nasm" => "Assembly",
        "astro" => "Astro",
        "awk" => "AWK",

        // B
        "bat" | "cmd" => "Batch",

        // C
        "c" | "h" => "C",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "ipp" | "inl" => "C++",
        "cs" => "C#",
        "cairo" => "Cairo",
        "clj" | "cljs" | "cljc" | "edn" => "Clojure",
        "cmake" => "CMake",
        "cob" | "cbl" | "cpy" => "COBOL",
        "coffee" | "litcoffee" => "CoffeeScript",
        "cr" => "Crystal",
        "css" | "scss" | "sass" | "less" | "styl" | "stylus" => "CSS",
        "csv" | "tsv" => "CSV",
        "cu" | "cuh" => "CUDA",

        // D
        "d" => "D",
        "dart" => "Dart",
        "dhall" => "Dhall",
        "dockerfile" => "Docker",

        // E
        "ejs" => "EJS",
        "elm" => "Elm",
        "ex" | "exs" | "heex" | "leex" => "Elixir",
        "erl" | "hrl" => "Erlang",

        // F
        "f" | "f90" | "f95" | "f03" | "f08" | "for" => "Fortran",
        "fs" | "fsi" | "fsx" => "F#",

        // G
        "gd" | "gdscript" => "GDScript",
        "gleam" => "Gleam",
        "glsl" | "vert" | "frag" | "geom" | "comp" => "GLSL",
        "go" => "Go",
        "gradle" => "Gradle",
        "graphql" | "gql" => "GraphQL",
        "groovy" | "gvy" => "Groovy",

        // H
        "haml" => "HAML",
        "hbs" | "handlebars" => "Handlebars",
        "hs" | "lhs" => "Haskell",
        "hcl" => "HCL",
        "hlsl" => "HLSL",
        "html" | "htm" | "xhtml" => "HTML",
        "http" | "rest" => "HTTP",

        // I-J
        "idris" | "idr" => "Idris",
        "ipynb" => "Jupyter",
        "java" => "Java",
        "jinja" | "jinja2" | "j2" => "Jinja",
        "jl" => "Julia",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "json" | "jsonc" | "jsonl" | "json5" | "geojson" => "JSON",

        // K
        "kdl" => "KDL",
        "kt" | "kts" => "Kotlin",

        // L
        "latex" | "tex" | "ltx" | "sty" => "LaTeX",
        "liquid" => "Liquid",
        "lisp" | "cl" | "el" | "elisp" => "Lisp",
        "lock" => "Lock",
        "lua" => "Lua",

        // M
        "m" | "mm" => "Objective-C",
        "makefile" | "mk" => "Makefile",
        "mat" | "matlab" => "MATLAB",
        "md" | "mdx" | "rst" | "adoc" | "asciidoc" => "Markdown",
        "ml" | "mli" => "OCaml",
        "mojo" => "Mojo",
        "move" => "Move",
        "mustache" => "Mustache",

        // N
        "nim" => "Nim",
        "nix" => "Nix",
        "njk" | "nunjucks" => "Nunjucks",
        "nu" => "Nushell",

        // O
        "odin" => "Odin",

        // P
        "pas" | "pp" | "lpr" => "Pascal",
        "pdf" => "PDF",
        "perl" | "pl" | "pm" | "t" | "pod" => "Perl",
        "php" | "phtml" | "phps" => "PHP",
        "prisma" => "Prisma",
        "proto" => "Protobuf",
        "ps1" | "psm1" | "psd1" => "PowerShell",
        "pug" | "jade" => "Pug",
        "purs" => "PureScript",
        "py" | "pyi" | "pyw" | "pyx" => "Python",

        // R
        "r" => "R",
        "rkt" | "scrbl" => "Racket",
        "re" | "rei" => "ReScript",
        "rmd" => "R Markdown",
        "robot" => "Robot Framework",
        "rs" => "Rust",

        // S
        "scala" | "sc" => "Scala",
        "scm" | "ss" => "Scheme",
        "sh" | "bash" | "zsh" | "fish" | "ksh" | "csh" | "tcsh" => "Shell",
        "slim" => "Slim",
        "snap" => "Snapshot",
        "sol" => "Solidity",
        "sql" | "psql" | "mysql" | "pgsql" | "plsql" => "SQL",
        "sv" | "svh" | "verilog" => "Verilog",
        "svelte" => "Svelte",
        "swift" => "Swift",

        // T
        "tcl" | "tk" => "Tcl",
        "tf" | "tfvars" => "Terraform",
        "toml" => "TOML",
        "ts" | "tsx" | "mts" | "cts" => "TypeScript",
        "twig" => "Twig",
        "txt" | "text" | "log" => "Text",

        // V
        "v" => "V",
        "vala" | "vapi" => "Vala",
        "vb" => "Visual Basic",
        "vhd" | "vhdl" => "VHDL",
        "vue" => "Vue",

        // W
        "wasm" | "wat" => "WebAssembly",
        "wgsl" => "WGSL",

        // X-Y
        "xaml" => "XAML",
        "xml" | "xsl" | "xslt" | "xsd" | "plist" | "rss" | "atom" => "XML",
        "yaml" | "yml" => "YAML",

        // Z
        "zig" => "Zig",

        // Binary & media
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "ico" | "bmp" | "tiff" | "avif" | "svg" => {
            "Image"
        }
        "mp3" | "wav" | "ogg" | "flac" | "aac" | "m4a" => "Audio",
        "mp4" | "webm" | "avi" | "mov" | "mkv" | "flv" => "Video",
        "ttf" | "otf" | "woff" | "woff2" | "eot" => "Font",
        "zip" | "gz" | "tar" | "bz2" | "xz" | "7z" | "rar" | "zst" => "Archive",

        _ => "Other",
    }
}

pub(crate) fn from_glob_pattern(pattern: &str) -> Option<&'static str> {
    let filename = pattern.rsplit('/').next().unwrap_or(pattern);
    if let Some(brace_start) = filename.find('{')
        && let Some(brace_end) = filename.find('}')
    {
        let inner = &filename[brace_start + 1..brace_end];
        for part in inner.split(',') {
            let ext = part.trim().trim_start_matches('.');
            let lang = for_extension(&ext.to_lowercase());
            if lang != "Other" {
                return Some(lang);
            }
        }
        return None;
    }
    from_path(pattern)
}

pub(crate) fn from_path(path: &str) -> Option<&'static str> {
    let filename = path.rsplit('/').next().unwrap_or(path);
    let filename_lower = filename.to_lowercase();

    match filename_lower.as_str() {
        "makefile" | "gnumakefile" | "justfile" => return Some("Makefile"),
        "dockerfile" | "containerfile" => return Some("Docker"),
        "gemfile" | "rakefile" | "vagrantfile" => return Some("Ruby"),
        "cmakelists.txt" => return Some("CMake"),
        _ => {}
    }

    let ext = filename.rsplit('.').next()?.to_lowercase();
    if ext == filename_lower {
        return Some("Other");
    }

    Some(for_extension(&ext))
}

pub(crate) fn extension_from_path(path: &str) -> Option<String> {
    let filename = path.rsplit('/').next().unwrap_or(path).trim();
    let filename_lower = filename.to_lowercase();

    match filename_lower.as_str() {
        "makefile" | "gnumakefile" | "justfile" | "dockerfile" | "containerfile" | "gemfile"
        | "rakefile" | "vagrantfile" | "cmakelists.txt" => return None,
        _ => {}
    }

    // A leading-dot name with no further dot is a dotfile (`.gitignore`,
    // `.bash_profile`, `.node-version`), not a file with an extension —
    // treating its whole name as the "extension" pollutes the Languages panel.
    if let Some(rest) = filename_lower.strip_prefix('.')
        && !rest.contains('.')
    {
        return None;
    }

    let ext = filename_lower.rsplit('.').next()?;
    if ext == filename_lower {
        return None;
    }
    // A real extension is a short ASCII-alphanumeric token. Reject anything
    // carrying path / glob / punctuation so a malformed input can never leak
    // a row like `.github)` or `.json"` into `extension_usage`.
    if ext.is_empty() || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(ext.to_string())
}

/// Lowercased extension of a single filename fragment, or `None`. Rejects
/// anything that isn't a clean ASCII-alphanumeric token after the last dot
/// so brace/glob punctuation (`{js,json}`, `ts}`, `}`, `config.*`) can never
/// leak into `extension_usage`.
fn clean_ext(name: &str) -> Option<String> {
    let dot = name.rfind('.')?;
    let ext = name[dot + 1..].trim().to_lowercase().replace('*', "");
    if ext.is_empty() || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(ext)
}

pub(crate) fn parse_extensions_from_glob(pattern: &str) -> Vec<String> {
    // `.trim()` drops trailing newlines/whitespace that some patterns carry;
    // left in place they made `ends_with('}')` fail and leaked the whole
    // `{...}` blob as one "extension".
    let filename = pattern.rsplit('/').next().unwrap_or(pattern).trim();

    // Brace alternation `{a,b,c}`. Find the FIRST `{`/`}` (a single `rfind('.')`
    // lands inside alternatives that contain a dot, e.g. `{tsconfig.json,*.ts}`,
    // yielding garbage like `ts}` / `}`).
    if let Some(open) = filename.find('{')
        && let Some(close_rel) = filename[open..].find('}')
    {
        let close = open + close_rel;
        let after_brace = &filename[close + 1..];
        // A dot after the closing brace means the real extension lives there
        // and the braces are just an infix alternation (`*.{test,spec}.tsx`).
        if after_brace.contains('.') {
            return clean_ext(after_brace).into_iter().collect();
        }
        // Otherwise each alternative is its own filename fragment carrying the
        // extension; resolve `prefix + alternative` so `*.{js,json}` and
        // `{tsconfig.json,*.ts}` both work. Dedup so one glob counts an
        // extension once even when two alternatives share it.
        let prefix = &filename[..open];
        let inner = &filename[open + 1..close];
        let mut out: Vec<String> = Vec::new();
        for alt in inner.split(',') {
            if let Some(ext) = clean_ext(&format!("{prefix}{}", alt.trim()))
                && !out.contains(&ext)
            {
                out.push(ext);
            }
        }
        return out;
    }

    clean_ext(filename).into_iter().collect()
}

#[cfg(test)]
mod glob_ext_tests {
    use super::parse_extensions_from_glob as p;

    #[test]
    fn plain_brace_alternation_splits_into_extensions() {
        assert_eq!(p("**/*.{ts,tsx}"), vec!["ts", "tsx"]);
        assert_eq!(
            p("src/**/*.{js,jsx,ts,tsx}"),
            vec!["js", "jsx", "ts", "tsx"]
        );
        assert_eq!(p("{*.yml,*.yaml}"), vec!["yml", "yaml"]);
    }

    #[test]
    fn non_brace_pattern_takes_trailing_extension() {
        assert_eq!(p("**/*.rs"), vec!["rs"]);
        assert_eq!(p("Cargo.toml"), vec!["toml"]);
    }

    #[test]
    fn brace_followed_by_real_extension_uses_that_extension() {
        // `*.{test,spec}.tsx` matches `foo.test.tsx`; the extension is `tsx`,
        // not the infix alternation `test`/`spec`.
        assert_eq!(p("**/*.{test,spec}.tsx"), vec!["tsx"]);
    }

    #[test]
    fn dotted_brace_alternatives_do_not_leak_braces() {
        // Regression: `rfind('.')` used to land inside the alternatives and
        // emit `ts}` / `json}`. Each alternative is resolved on its own.
        assert_eq!(p("config/{tsconfig.json,*.ts}"), vec!["json", "ts"]);
    }

    #[test]
    fn star_alternative_yields_no_garbage_extension() {
        // `config.*` has no concrete extension → dropped, not emitted as `}`.
        assert_eq!(p("*.{js,json,config.*}"), vec!["js", "json"]);
    }

    #[test]
    fn trailing_whitespace_does_not_leak_the_brace_blob() {
        // A trailing newline once defeated `ends_with('}')` and leaked
        // `{js,json}` verbatim as one extension.
        assert_eq!(p("*.{js,json}\n"), vec!["js", "json"]);
        assert_eq!(p("*.{js,json} "), vec!["js", "json"]);
    }

    #[test]
    fn duplicate_extension_across_alternatives_is_counted_once() {
        assert_eq!(p("**/*.{spec.ts,test.ts}"), vec!["ts"]);
    }

    #[test]
    fn special_filenames_have_no_extension() {
        assert!(p("**/{Makefile,Dockerfile}").is_empty());
    }
}

#[cfg(test)]
mod path_ext_tests {
    use super::extension_from_path as e;

    #[test]
    fn normal_files_yield_their_extension() {
        assert_eq!(e("src/main.rs").as_deref(), Some("rs"));
        assert_eq!(e("a/b/App.tsx").as_deref(), Some("tsx"));
        assert_eq!(e("data.tar.gz").as_deref(), Some("gz"));
    }

    #[test]
    fn dotfiles_have_no_extension() {
        // Regression: `.bash_profile` / `.node-version` once surfaced their
        // whole name as a bogus extension row.
        assert_eq!(e(".bash_profile"), None);
        assert_eq!(e("~/.node-version"), None);
        assert_eq!(e("repo/.gitignore"), None);
        // A dotfile WITH a real extension still resolves it.
        assert_eq!(e(".eslintrc.js").as_deref(), Some("js"));
    }

    #[test]
    fn punctuation_is_rejected() {
        // Malformed inputs must not leak `.github)` / `.json"` rows.
        assert_eq!(e("foo.github)"), None);
        assert_eq!(e("x.json\""), None);
        assert_eq!(e("y.config-backup"), None);
    }

    #[test]
    fn extensionless_files_are_none() {
        assert_eq!(e("README"), None);
        assert_eq!(e("LICENSE"), None);
    }
}
