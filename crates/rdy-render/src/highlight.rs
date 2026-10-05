//! Build-time syntax highlighting: the code as escaped HTML with tokens wrapped in the Tide
//! token classes (`tk-k` keyword, `tk-s` string, `tk-c` comment, `tk-n` number, `tk-f` function
//! or key, `tk-t` type, `tk-d` decorator, attribute or variable).
//!
//! Each language is one entry in [`LANGS`]: an ordered rule table. The rules of a language are
//! joined into one alternation and run left to right over the whole text, so at each position
//! the earliest rule that matches wins, and among rules that start at the same place the first
//! listed (comment, then string, then number, then words). Every rule has exactly one capture
//! group, the token. Text a rule matches around its token (indentation, a list marker, the `:`
//! after a key) is how it looks ahead and behind, which the `regex` crate cannot do; that text
//! is written out as plain. Text is escaped as it is emitted and the source is never escaped
//! first, so the patterns never see entities.
use crate::html::escape;
use regex::Regex;
use std::fmt::Write;
use std::sync::OnceLock;

/// How a rule's token is classed.
#[derive(Clone, Copy)]
enum Class {
    /// Always this class.
    Fixed(&'static str),
    /// Decided from the token's text; an empty class leaves the word plain.
    Word(fn(&str) -> &'static str),
}

use Class::{Fixed, Word};

struct Rule {
    /// The pattern, with exactly one capture group (the token). [`KEYWORDS`] stands for the
    /// language's keyword list.
    pat: &'static str,
    class: Class,
}

const fn rule(pat: &'static str, class: &'static str) -> Rule {
    Rule {
        pat,
        class: Fixed(class),
    }
}

/// The placeholder pattern for the language's keywords (whole words).
const KEYWORDS: &str = "@keywords";

struct Lang {
    /// Every name the language goes by in a code block's `lang`, lower case.
    names: &'static [&'static str],
    /// What a reader calls it.
    human: &'static str,
    keywords: &'static [&'static str],
    rules: &'static [Rule],
}

const SHELL_KEYWORDS: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "in", "do", "done", "while", "case", "esac",
    "function", "export", "return", "exit",
];

fn shell_first_word(w: &str) -> &'static str {
    if SHELL_KEYWORDS.contains(&w) {
        "tk-k"
    } else {
        "tk-f"
    }
}

fn shell_word(w: &str) -> &'static str {
    if SHELL_KEYWORDS.contains(&w) {
        "tk-k"
    } else {
        ""
    }
}

const LANGS: &[Lang] = &[
    Lang {
        names: &["python", "py", "python3"],
        human: "Python",
        keywords: &[
            "match", "case", "if", "else", "elif", "for", "in", "while", "def", "class", "return",
            "import", "from", "as", "pass", "raise", "not", "and", "or", "is", "None", "True",
            "False", "with", "try", "except", "finally", "lambda", "yield", "async", "await",
            "del", "global", "nonlocal", "assert", "break", "continue",
        ],
        rules: &[
            rule(r"(#[^\n]*)", "tk-c"),
            rule(
                concat!(
                    r#"((?:\b[fFrRbBuU]{1,2})?(?:"#,
                    r#"'''(?:\\(?s:.)|[^\\])*?(?:'''|\z)"#,
                    r#"|"""(?:\\(?s:.)|[^\\])*?(?:"""|\z)"#,
                    r#"|'(?:\\(?s:.)|[^'\\\n])*'"#,
                    r#"|"(?:\\(?s:.)|[^"\\\n])*")"#,
                    r")"
                ),
                "tk-s",
            ),
            rule(r"^[ \t]*(@[A-Za-z0-9_.]+)", "tk-d"),
            rule(
                r"\b(0[xXoObB][0-9a-fA-F_]+|\d[\d_]*(?:\.\d+)?(?:[eE][+-]?\d+)?[jJ]?)\b",
                "tk-n",
            ),
            rule(KEYWORDS, "tk-k"),
            rule(r"\b([A-Za-z_][A-Za-z0-9_]*)\(", "tk-f"),
            rule(r"\b([A-Z][A-Za-z0-9_]*)", "tk-t"),
        ],
    },
    Lang {
        names: &["yaml", "yml"],
        human: "YAML",
        keywords: &["true", "false", "null"],
        rules: &[
            rule(r"\B(#[^\n]*)", "tk-c"),
            rule(r#"("(?:\\.|[^"\\\n])*")"#, "tk-s"),
            rule(r"(\B'(?:[^'\n]|'')*')", "tk-s"),
            rule(r"^[ \t]*(?:-[ \t]+)*([A-Za-z0-9_.-]+)[ \t]*:", "tk-f"),
            rule(
                r"\b(0[xX][0-9a-fA-F_]+|\d[\d_]*(?:\.\d+)*(?:[eE][+-]?\d+)?)\b",
                "tk-n",
            ),
            rule(KEYWORDS, "tk-k"),
            rule(r"\B(~)[ \t]*$", "tk-k"),
        ],
    },
    Lang {
        names: &["json"],
        human: "JSON",
        keywords: &["true", "false", "null"],
        rules: &[
            rule(r#"("(?:\\.|[^"\\\n])*")\s*:"#, "tk-f"),
            rule(r#"("(?:\\.|[^"\\\n])*")"#, "tk-s"),
            rule(r"(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)", "tk-n"),
            rule(KEYWORDS, "tk-k"),
        ],
    },
    Lang {
        names: &["rust", "rs"],
        human: "Rust",
        keywords: &[
            "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn",
            "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
            "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
            "unsafe", "use", "where", "while", "async", "await", "dyn",
        ],
        rules: &[
            rule(r"(//[^\n]*)", "tk-c"),
            rule(r"((?s:/\*.*?(?:\*/|\z)))", "tk-c"),
            // Raw strings: the `regex` crate has no back-references, so up to three hashes.
            rule(
                concat!(
                    r##"(\b(?:br|r)(?:"[^"]*""##,
                    r###"|#"(?s:.*?)"#"###,
                    r####"|##"(?s:.*?)"##"####,
                    r#####"|###"(?s:.*?)"###)"#####,
                    r")"
                ),
                "tk-s",
            ),
            rule(r#"((?:\bb)?"(?:\\(?s:.)|[^"\\])*")"#, "tk-s"),
            // A char needs its closing quote at once, which a lifetime never has.
            rule(
                r"('(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|[^\n])|[^'\\\n])')",
                "tk-s",
            ),
            rule(r"(#!?\[[^\]\n]*\])", "tk-d"),
            rule(
                r"\b((?:0x[0-9a-fA-F_]+|0o[0-7_]+|0b[01_]+|\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d+)?)(?:[iu](?:8|16|32|64|128|size)|f32|f64)?)\b",
                "tk-n",
            ),
            rule(KEYWORDS, "tk-k"),
            rule(r"\b([A-Za-z_][A-Za-z0-9_]*!)[(\[{]", "tk-f"),
            rule(r"\b([A-Za-z_][A-Za-z0-9_]*)\(", "tk-f"),
            rule(r"\b([A-Z][A-Za-z0-9_]*)", "tk-t"),
        ],
    },
    Lang {
        names: &["sh", "bash", "zsh", "shell", "console"],
        human: "Shell",
        keywords: &[],
        rules: &[
            rule(r"(\$\{[^}\n]*\}|\$\w+|\$[#?!@*$])", "tk-d"),
            rule(r"\B(#[^\n]*)", "tk-c"),
            rule(r#"("(?:\\(?s:.)|[^"\\])*")"#, "tk-s"),
            rule(r"(\B'[^']*')", "tk-s"),
            Rule {
                pat: r#"^[ \t]*(?:[$>][ \t]+)?([A-Za-z0-9_./~-][^\s"'$;|&()<>\\`#=]*)"#,
                class: Word(shell_first_word),
            },
            Rule {
                pat: r#"([A-Za-z0-9_./~-][^\s"'$;|&()<>\\`#=]*)"#,
                class: Word(shell_word),
            },
        ],
    },
    Lang {
        names: &["toml"],
        human: "TOML",
        keywords: &["true", "false"],
        rules: &[
            rule(r"\B(#[^\n]*)", "tk-c"),
            rule(
                concat!(
                    r#"("""(?:\\(?s:.)|[^\\])*?(?:"""|\z)"#,
                    r#"|'''(?s:.*?)(?:'''|\z)"#,
                    r#"|"(?:\\.|[^"\\\n])*")"#
                ),
                "tk-s",
            ),
            rule(r"(\B'[^'\n]*')", "tk-s"),
            rule(r#"^[ \t]*(\[\[?[A-Za-z0-9_. "'-]+\]\]?)"#, "tk-t"),
            rule(r#"^[ \t]*([A-Za-z0-9_.-]+|"[^"\n]*")[ \t]*="#, "tk-f"),
            rule(
                r"\b(0[xX][0-9a-fA-F_]+|\d[\d_]*(?:\.\d+)*(?:[eE][+-]?\d+)?)\b",
                "tk-n",
            ),
            rule(KEYWORDS, "tk-k"),
        ],
    },
];

/// Languages a reader names that have no highlighting.
const PLAIN_NAMES: &[(&[&str], &str)] = &[
    (&["peg"], "Grammar"),
    (&["mermaid"], "Mermaid"),
    (&["d2"], "D2"),
    (&["", "text", "txt", "plain"], "Text"),
    (&["html"], "HTML"),
    (&["css"], "CSS"),
    (&["js", "javascript"], "JavaScript"),
    (&["ts", "typescript"], "TypeScript"),
    (&["sql"], "SQL"),
    (&["md", "markdown"], "Markdown"),
];

fn lookup(name: &str) -> Option<usize> {
    LANGS.iter().position(|l| l.names.contains(&name))
}

/// One language's rules joined into a single pattern.
fn compile(lang: &Lang) -> Regex {
    let keywords = format!(r"\b(?:{})\b", lang.keywords.join("|"));
    let alternatives: Vec<String> = lang
        .rules
        .iter()
        .map(|r| {
            let pat = if r.pat == KEYWORDS {
                // The keyword rule's one group is the whole word.
                format!("({keywords})")
            } else {
                r.pat.to_string()
            };
            // Boundaries are ASCII, like the identifier classes: the Unicode tables would
            // make every language's automaton larger for nothing.
            format!(
                "(?:{})",
                pat.replace(r"\b", r"(?-u:\b)").replace(r"\B", r"(?-u:\B)")
            )
        })
        .collect();
    let re = Regex::new(&format!("(?m){}", alternatives.join("|")))
        .expect("the highlight rule tables are valid patterns");
    debug_assert_eq!(
        re.captures_len(),
        lang.rules.len() + 1,
        "one group per rule"
    );
    re
}

fn compiled(i: usize) -> &'static Regex {
    static CACHE: [OnceLock<Regex>; LANGS.len()] = [const { OnceLock::new() }; LANGS.len()];
    CACHE[i].get_or_init(|| compile(&LANGS[i]))
}

/// The code as HTML: escaped, with tokens wrapped in `<span class="tk-…">`. A language that is
/// not in the table comes back as escaped text only.
pub fn html(lang: &str, code: &str) -> String {
    let Some(i) = lookup(&lang.trim().to_lowercase()) else {
        return escape(code);
    };
    let rules = LANGS[i].rules;
    let mut out = String::with_capacity(code.len() + code.len() / 2);
    let mut last = 0;
    for caps in compiled(i).captures_iter(code) {
        let Some((rule, token)) = (1..caps.len()).find_map(|g| caps.get(g).map(|m| (g - 1, m)))
        else {
            continue;
        };
        let class = match rules[rule].class {
            Fixed(c) => c,
            Word(f) => f(token.as_str()),
        };
        // The text before the token includes what the rule matched around it.
        out.push_str(&escape(&code[last..token.start()]));
        if class.is_empty() {
            out.push_str(&escape(token.as_str()));
        } else {
            let _ = write!(
                out,
                "<span class=\"{class}\">{}</span>",
                escape(token.as_str())
            );
        }
        last = token.end();
    }
    out.push_str(&escape(&code[last..]));
    out
}

/// The language as a reader names it, for the code label.
pub fn human_name(lang: &str) -> String {
    let name = lang.trim().to_lowercase();
    if let Some(i) = lookup(&name) {
        return LANGS[i].human.to_string();
    }
    if let Some((_, human)) = PLAIN_NAMES
        .iter()
        .find(|(names, _)| names.contains(&name.as_str()))
    {
        return (*human).to_string();
    }
    let mut chars = lang.trim().chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hl1_python_tokens_map_to_tide_classes() {
        let h = html(
            "python",
            "@dataclass\nclass Point:  # a point\n    def dist(self, other: 'Point') -> float:\n        return 1.5e3 if match else f\"{x}\"\n",
        );
        assert!(h.contains("<span class=\"tk-d\">@dataclass</span>"));
        assert!(h.contains("<span class=\"tk-k\">class</span> <span class=\"tk-t\">Point</span>:  <span class=\"tk-c\"># a point</span>"));
        assert!(h.contains("<span class=\"tk-k\">def</span> <span class=\"tk-f\">dist</span>("));
        assert!(
            h.contains("<span class=\"tk-s\">&#39;Point&#39;</span>")
                && h.contains("<span class=\"tk-n\">1.5e3</span>")
                && h.contains("<span class=\"tk-s\">f&quot;{x}&quot;</span>")
        );
        assert!(h.contains("<span class=\"tk-k\">match</span>"));
    }
    #[test]
    fn hl1_unknown_languages_are_escaped_only() {
        assert_eq!(
            html("peg", "a: <b> & \"c\""),
            "a: &lt;b&gt; &amp; &quot;c&quot;"
        );
        assert_eq!(html("", "<x>"), "&lt;x&gt;");
    }
    #[test]
    fn hl1_strings_and_comments_swallow_markup_and_keywords() {
        let h = html("rust", "let s = \"if <b> // not a comment\"; // if\n");
        assert!(
            h.contains("<span class=\"tk-s\">&quot;if &lt;b&gt; // not a comment&quot;</span>")
        );
        assert!(h.contains("<span class=\"tk-c\">// if</span>"));
        assert_eq!(h.matches("tk-k").count(), 1, "only `let`: {h}");
    }
    #[test]
    fn hl1_yaml_json_shell_toml_have_rules_and_names() {
        assert!(
            html("yaml", "name: x # c\nok: true").contains("<span class=\"tk-f\">name</span>:")
                && html("yaml", "ok: true").contains("tk-k\">true")
        );
        assert!(
            html("json", "{\"a\": 1}").contains(
                "<span class=\"tk-f\">&quot;a&quot;</span>: <span class=\"tk-n\">1</span>"
            )
        );
        assert!(html("sh", "echo $HOME # hi").contains("tk-d\">$HOME"));
        assert!(html("toml", "[pkg]\nname = \"x\"").contains("tk-t\">[pkg]"));
        for (l, n) in [
            ("py", "Python"),
            ("Python3", "Python"),
            ("yml", "YAML"),
            ("rs", "Rust"),
            ("console", "Shell"),
            ("peg", "Grammar"),
            ("foo", "Foo"),
            ("", "Text"),
        ] {
            assert_eq!(human_name(l), n);
        }
    }
    #[test]
    fn hl1_output_is_well_formed_and_round_trips_the_text() {
        let src = "x = 'a\\'b' # \"q\"\nprint(x)\n";
        let h = html("python", src);
        let text = h
            .replace("<span class=\"tk-k\">", "")
            .replace("<span class=\"tk-s\">", "")
            .replace("<span class=\"tk-c\">", "")
            .replace("<span class=\"tk-f\">", "")
            .replace("<span class=\"tk-n\">", "")
            .replace("<span class=\"tk-t\">", "")
            .replace("<span class=\"tk-d\">", "")
            .replace("</span>", "");
        assert_eq!(
            text,
            src.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
                .replace('\'', "&#39;")
        );
        assert!(
            h.contains("<span class=\"tk-s\">&#39;a\\&#39;b&#39;</span>"),
            "an escaped quote does not end the string: {h}"
        );
    }
    #[test]
    fn hl1_two_thousand_lines_highlight_in_well_under_a_second() {
        let src = "def f(x):\n    return x  # c\n".repeat(1000);
        let t = std::time::Instant::now();
        let h = html("python", &src);
        assert!(t.elapsed().as_millis() < 300, "{:?}", t.elapsed());
        assert!(h.len() > src.len());
    }
    #[test]
    fn hl1_every_language_compiles_with_one_group_per_rule() {
        for (i, l) in LANGS.iter().enumerate() {
            assert_eq!(compiled(i).captures_len(), l.rules.len() + 1, "{}", l.human);
            assert_eq!(html(l.names[0], ""), "");
        }
    }
    #[test]
    fn hl1_rust_rules_separate_lifetimes_chars_macros_and_attributes() {
        let h = html(
            "rs",
            "#[derive(Debug)]\nfn f<'a>(x: &'a str) -> Vec<u8> { println!(\"{}\", 'x'); r#\"raw \" \"#; 0xFFu8 }\n/* if */",
        );
        assert!(
            h.contains("<span class=\"tk-d\">#[derive(Debug)]</span>"),
            "{h}"
        );
        assert!(h.contains("<span class=\"tk-k\">fn</span> f&lt;"), "{h}");
        assert!(
            h.contains("&lt;&#39;a&gt;") && h.contains("&amp;&#39;a "),
            "a lifetime is not a char: {h}"
        );
        assert!(
            h.contains("<span class=\"tk-f\">println!</span>(")
                && h.contains("<span class=\"tk-s\">&#39;x&#39;</span>"),
            "{h}"
        );
        assert!(
            h.contains("<span class=\"tk-s\">r#&quot;raw &quot; &quot;#</span>"),
            "{h}"
        );
        assert!(
            h.contains("<span class=\"tk-n\">0xFFu8</span>")
                && h.contains("<span class=\"tk-c\">/* if */</span>"),
            "{h}"
        );
        assert!(h.contains("<span class=\"tk-t\">Vec</span>"), "{h}");
    }
    #[test]
    fn hl1_python_triple_quotes_and_words_inside_words() {
        let h = html(
            "python",
            "\"\"\"doc 'x'\n# not\n\"\"\"\nundef = classy + x1\nobj.run(0x1F)\n",
        );
        assert!(h.contains("<span class=\"tk-s\">&quot;&quot;&quot;doc &#39;x&#39;\n# not\n&quot;&quot;&quot;</span>"), "{h}");
        assert!(
            !h.contains("tk-k") && !h.contains("tk-c"),
            "no keyword inside undef or classy: {h}"
        );
        assert!(
            h.contains("<span class=\"tk-f\">run</span>(<span class=\"tk-n\">0x1F</span>)"),
            "{h}"
        );
    }
    #[test]
    fn hl1_yaml_toml_shell_context_rules() {
        let y = html(
            "yaml",
            "items:\n  - id: 1.2.3\n    note: 'it''s'  # c\nurl: http://x\nv: ~\n",
        );
        assert!(
            y.contains("<span class=\"tk-f\">items</span>:")
                && y.contains("<span class=\"tk-f\">id</span>: <span class=\"tk-n\">1.2.3</span>"),
            "{y}"
        );
        assert!(
            y.contains("<span class=\"tk-s\">&#39;it&#39;&#39;s&#39;</span>")
                && y.contains("<span class=\"tk-c\"># c</span>")
                && y.contains("<span class=\"tk-k\">~</span>"),
            "{y}"
        );
        let t = html("toml", "[[bin]]\nname = \"x\" # c\nn = 5\nok = true\n");
        assert!(
            t.contains("<span class=\"tk-t\">[[bin]]</span>")
                && t.contains("<span class=\"tk-f\">name</span> =")
                && t.contains("tk-n\">5")
                && t.contains("tk-k\">true"),
            "{t}"
        );
        let s = html(
            "console",
            "$ cargo test --release # go\nif [ -n \"$X\" ]; then echo don't; fi\n",
        );
        assert!(s.contains("$ <span class=\"tk-f\">cargo</span> test --release <span class=\"tk-c\"># go</span>"), "{s}");
        assert!(s.contains("<span class=\"tk-k\">if</span>") && s.contains("<span class=\"tk-k\">then</span> echo don&#39;t; <span class=\"tk-k\">fi</span>"), "{s}");
    }
}
