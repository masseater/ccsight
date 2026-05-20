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
    let filename = path.rsplit('/').next().unwrap_or(path);
    let filename_lower = filename.to_lowercase();

    match filename_lower.as_str() {
        "makefile" | "gnumakefile" | "justfile" | "dockerfile" | "containerfile" | "gemfile"
        | "rakefile" | "vagrantfile" | "cmakelists.txt" => return None,
        _ => {}
    }

    let ext = filename.rsplit('.').next()?.to_lowercase();
    if ext == filename_lower {
        return None;
    }
    Some(ext)
}

pub(crate) fn parse_extensions_from_glob(pattern: &str) -> Vec<String> {
    let filename = pattern.rsplit('/').next().unwrap_or(pattern);
    let Some(dot_pos) = filename.rfind('.') else {
        return vec![];
    };
    let after_dot = &filename[dot_pos + 1..];
    if after_dot.is_empty() {
        return vec![];
    }

    if after_dot.starts_with('{') && after_dot.ends_with('}') {
        let inner = &after_dot[1..after_dot.len() - 1];
        return inner
            .split(',')
            .filter_map(|s| {
                let s = s.trim().to_lowercase();
                let s = s.replace('*', "");
                if s.is_empty() || s.contains('{') || s.contains('}') {
                    None
                } else {
                    Some(s)
                }
            })
            .collect();
    }

    let ext = after_dot.to_lowercase().replace('*', "");
    if ext.is_empty() || ext == filename.to_lowercase() {
        return vec![];
    }
    vec![ext]
}
