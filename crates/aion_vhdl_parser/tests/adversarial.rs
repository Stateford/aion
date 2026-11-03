//! Adversarial-input corpus for the VHDL lexer and parser.
//!
//! Every case here is malformed (or at least unusual) VHDL. The only property
//! checked is that lexing + parsing *terminates* with a bounded number of
//! diagnostics and never trips the parser's internal no-progress backstop.
//! A worker thread runs the cases while the test thread acts as a watchdog:
//! if any single case takes longer than [`CASE_TIMEOUT`], the test fails and
//! names the offending input instead of hanging CI.
//!
//! The corpus is built from:
//! - every token-prefix of several valid sample sources (truncation),
//! - every sample with one token deleted,
//! - every VHDL keyword (and common punctuation) injected at a spread of
//!   token positions in every sample,
//! - deterministic pseudo-random token soup from a seeded LCG.

use aion_common::Interner;
use aion_diagnostics::{Diagnostic, DiagnosticSink};
use aion_source::FileId;
use aion_vhdl_parser::lexer;
use aion_vhdl_parser::parser::VhdlParser;
use aion_vhdl_parser::Token;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Maximum wall-clock time a single case may take before it is declared hung.
const CASE_TIMEOUT: Duration = Duration::from_secs(5);

/// Substring of the diagnostic the parser emits when its no-progress
/// backstop fires. Seeing it means some loop failed to consume input.
const BACKSTOP_MESSAGE: &str = "parser made no progress";

/// A small entity/architecture pair with ports, generics, a clocked process,
/// a case statement, and a component instantiation.
const SAMPLE_COUNTER: &str = r#"
library ieee;
use ieee.std_logic_1164.all;
use ieee.numeric_std.all;

entity counter is
    generic (
        WIDTH : integer := 8;
        RESET_VAL : natural := 0
    );
    port (
        clk   : in  std_logic;
        rst   : in  std_logic;
        en    : in  std_logic;
        count : out std_logic_vector(WIDTH - 1 downto 0)
    );
end entity counter;

architecture rtl of counter is
    signal cnt : unsigned(WIDTH - 1 downto 0) := (others => '0');
    type state_t is (idle, run, done);
    signal state : state_t;
begin
    process(clk, rst)
        variable tmp : integer := 0;
    begin
        if rst = '1' then
            cnt <= (others => '0');
        elsif rising_edge(clk) then
            case state is
                when idle => state <= run;
                when run | done =>
                    if en = '1' then
                        cnt <= cnt + 1;
                    end if;
                when others => null;
            end case;
        end if;
    end process;

    count <= std_logic_vector(cnt);

    u0 : entity work.sub(rtl)
        generic map (WIDTH => WIDTH)
        port map (a => clk, b => open, c => count(0));
end architecture rtl;
"#;

/// A package and package body with records, arrays, subtypes, constants,
/// functions, procedures, aliases, and attributes.
const SAMPLE_PACKAGE: &str = r#"
package types_pkg is
    type pixel_t is record
        r, g, b : std_logic_vector(7 downto 0);
        valid   : std_logic;
    end record;
    type mem_t is array (0 to 15) of std_logic_vector(7 downto 0);
    subtype byte_t is std_logic_vector(7 downto 0);
    constant ZERO : byte_t := x"00";
    type word_range is range 0 to 65535;
    attribute keep : boolean;
    function add_one(x : integer) return integer;
    procedure reset_pixel(signal p : out pixel_t);
    component sub is
        generic (W : integer := 4);
        port (a : in std_logic; b : out std_logic);
    end component;
end package types_pkg;

package body types_pkg is
    function add_one(x : integer) return integer is
        variable y : integer;
    begin
        y := x + 1;
        return y;
    end function add_one;

    procedure reset_pixel(signal p : out pixel_t) is
    begin
        p.valid <= '0';
    end procedure;

    alias low_nibble : std_logic_vector(3 downto 0) is ZERO(3 downto 0);
end package body types_pkg;
"#;

/// An architecture exercising generate statements, loops, waits, asserts,
/// and assorted sequential statements.
const SAMPLE_GENERATE: &str = r#"
entity tb is end entity;

architecture sim of tb is
    signal clk : std_logic := '0';
    signal bus_s : std_logic_vector(3 downto 0);
begin
    gen : for i in 0 to 3 generate
        bus_s(i) <= clk after 1 ns;
    end generate gen;

    cond : if WIDTH > 4 generate
        assert false report "wide" severity note;
    end generate;

    stim : process
    begin
        for j in 0 to 10 loop
            clk <= not clk;
            wait for 5 ns;
            next when j = 3;
            exit when j = 9;
        end loop;
        while clk = '0' loop
            wait until clk = '1';
        end loop;
        loop
            wait on clk, bus_s;
            exit;
        end loop;
        report "done" severity failure;
        assert bus_s = "0000";
        wait;
    end process stim;
end architecture sim;
"#;

/// A package and architecture exercising aggregate choice lists, named
/// association in calls, unconstrained (`range <>`) arrays, VHDL-2008
/// multi-level and `(open)` constraints, and resolution indications.
const SAMPLE_AGGREGATES: &str = r#"
package agg_pkg is
    type mem_t is array (natural range <>) of std_logic_vector;
    subtype mem4_t is mem_t(0 to 3)(7 downto 0);
    subtype memx_t is mem_t(open)(15 downto 0);
    subtype rsl is resolved std_ulogic;
    subtype rslv is (resolved) std_ulogic_vector;
end package agg_pkg;

architecture rtl of agg is
begin
    process (all)
        variable v : std_logic_vector(7 downto 0);
        variable u : unsigned(15 downto 0);
    begin
        v := (1 | 3 => '1', 7 downto 6 => d, others => '0');
        u := resize(arg => to_unsigned(arg => 3, size => 8), new_size => 16);
        write(l, value => v, justified => open);
        case sel is
            when 0 | 2 to 3 => v := ('1', others => '0');
            when others => null;
        end case;
    end process;
end architecture rtl;
"#;

/// Every reserved word the lexer recognizes as a keyword.
const KEYWORDS: &[&str] = &[
    "abs",
    "access",
    "after",
    "alias",
    "all",
    "and",
    "architecture",
    "array",
    "assert",
    "attribute",
    "begin",
    "block",
    "body",
    "buffer",
    "bus",
    "case",
    "component",
    "configuration",
    "constant",
    "context",
    "default",
    "disconnect",
    "downto",
    "else",
    "elsif",
    "end",
    "entity",
    "exit",
    "file",
    "for",
    "force",
    "function",
    "generate",
    "generic",
    "group",
    "guarded",
    "if",
    "impure",
    "in",
    "inertial",
    "inout",
    "is",
    "label",
    "library",
    "linkage",
    "literal",
    "loop",
    "map",
    "mod",
    "nand",
    "new",
    "next",
    "nor",
    "not",
    "null",
    "of",
    "on",
    "open",
    "or",
    "others",
    "out",
    "package",
    "parameter",
    "port",
    "postponed",
    "procedure",
    "process",
    "protected",
    "pure",
    "range",
    "record",
    "register",
    "reject",
    "release",
    "rem",
    "report",
    "return",
    "rol",
    "ror",
    "select",
    "severity",
    "signal",
    "shared",
    "sla",
    "sll",
    "sra",
    "srl",
    "subtype",
    "then",
    "to",
    "transport",
    "type",
    "unaffected",
    "units",
    "until",
    "use",
    "variable",
    "wait",
    "when",
    "while",
    "with",
    "xnor",
    "xor",
];

/// Punctuation and operators that commonly act as list separators or
/// terminators, injected alongside the keywords.
const PUNCTUATION: &[&str] = &[
    ";", ":", ",", "(", ")", ":=", "=>", "<=", "'", "|", ".", "<<", ">>", "??", "**", "&", "=",
    "<>",
];

/// Non-keyword lexemes used to fill out the random token soup.
const SOUP_EXTRAS: &[&str] = &[
    "a",
    "clk",
    "foo_bar",
    "\\ext id\\",
    "0",
    "42",
    "3.14",
    "16#FF#",
    "'0'",
    "'1'",
    "\"str\"",
    "x\"AB\"",
    "b\"0101\"",
    "10 ns",
    "-- comment\n",
    "/* block */",
    "@",
    "\"unterminated\n",
    "é",
];

/// The result of lexing and parsing one case.
struct Outcome {
    tokens: usize,
    diagnostics: Vec<Diagnostic>,
}

/// Lexes and parses `src`, returning the token count and all diagnostics.
fn lex_and_parse(src: &str) -> Outcome {
    let interner = Interner::new();
    let sink = DiagnosticSink::new();
    let file = FileId::from_raw(0);
    let tokens = lexer::lex(src, file, &sink);
    let ntokens = tokens.len();
    let mut parser = VhdlParser::new(tokens, src, file, &interner, &sink);
    let _ = parser.parse_design_file();
    Outcome {
        tokens: ntokens,
        diagnostics: sink.take_all(),
    }
}

/// Lexes `src` and returns its tokens (excluding the trailing EOF).
fn tokens_of(src: &str) -> Vec<Token> {
    let sink = DiagnosticSink::new();
    let mut toks = lexer::lex(src, FileId::from_raw(0), &sink);
    toks.pop(); // EOF
    assert!(sink.take_all().is_empty(), "sample must lex cleanly");
    toks
}

/// Runs every case on a worker thread, failing if any single case exceeds
/// [`CASE_TIMEOUT`], panics, emits an unbounded number of diagnostics, or
/// trips the parser's no-progress backstop.
fn run_corpus(name: &str, cases: Vec<String>) {
    let total = cases.len();
    // Kept on the watchdog side so a hung case can be named in the failure.
    let inputs = cases.clone();
    let (tx, rx) = mpsc::channel::<Result<(), String>>();
    let worker = thread::Builder::new()
        .name(format!("corpus-{name}"))
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            for src in &cases {
                let outcome = lex_and_parse(src);
                let errors = outcome.diagnostics.len();
                let bound = 8 * outcome.tokens + 64;
                let verdict = if let Some(d) = outcome
                    .diagnostics
                    .iter()
                    .find(|d| d.message.contains(BACKSTOP_MESSAGE))
                {
                    Err(format!(
                        "no-progress backstop fired ({}) for input:\n{src}",
                        d.message
                    ))
                } else if errors > bound {
                    Err(format!(
                        "{errors} diagnostics for {} tokens (bound {bound}) for input:\n{src}",
                        outcome.tokens
                    ))
                } else {
                    Ok(())
                };
                let failed = verdict.is_err();
                if tx.send(verdict).is_err() || failed {
                    return;
                }
            }
        })
        .expect("spawn corpus worker");

    for (index, input) in inputs.iter().enumerate() {
        match rx.recv_timeout(CASE_TIMEOUT) {
            Ok(Ok(())) => {}
            Ok(Err(msg)) => panic!("[{name}] case {index}/{total}: {msg}"),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!(
                    "[{name}] case {index}/{total} did not finish within {CASE_TIMEOUT:?} \
                     (parser hang) for input:\n{input}"
                )
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // Worker panicked; join to surface its panic message.
                match worker.join() {
                    Ok(()) => panic!("[{name}] worker exited early at case {index}/{total}"),
                    Err(e) => std::panic::resume_unwind(e),
                }
            }
        }
    }
    worker.join().expect("corpus worker panicked");
}

/// Returns the sample sources used to derive mutated cases.
fn samples() -> [&'static str; 4] {
    [
        SAMPLE_COUNTER,
        SAMPLE_PACKAGE,
        SAMPLE_GENERATE,
        SAMPLE_AGGREGATES,
    ]
}

/// Returns `src` with `insert` placed before token `at` (or at the end when
/// `at == toks.len()`); when `delete` is set, token `at` itself is dropped.
fn splice(src: &str, toks: &[Token], at: usize, insert: &str, delete: bool) -> String {
    let mut out = String::with_capacity(src.len() + insert.len() + 2);
    let cut = if at < toks.len() {
        toks[at].span.start as usize
    } else {
        src.len()
    };
    out.push_str(&src[..cut]);
    out.push(' ');
    out.push_str(insert);
    out.push(' ');
    let resume = if delete && at < toks.len() {
        toks[at].span.end as usize
    } else {
        cut
    };
    out.push_str(&src[resume..]);
    out
}

/// Minimal deterministic linear congruential generator (Knuth MMIX constants).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn samples_parse_cleanly() {
    for src in samples() {
        let outcome = lex_and_parse(src);
        assert!(
            outcome.diagnostics.is_empty(),
            "sample should parse with no diagnostics, got {:?}",
            outcome
                .diagnostics
                .iter()
                .map(|d| &d.message)
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn keyword_list_is_accurate() {
    for kw in KEYWORDS {
        let toks = tokens_of(kw);
        assert_eq!(toks.len(), 1, "{kw} should lex to one token");
        assert!(toks[0].kind.is_keyword(), "{kw} should lex as a keyword");
    }
}

#[test]
fn corpus_truncated_prefixes() {
    let mut cases = Vec::new();
    for src in samples() {
        let toks = tokens_of(src);
        cases.push(String::new());
        for tok in &toks {
            cases.push(src[..tok.span.end as usize].to_string());
        }
    }
    run_corpus("prefixes", cases);
}

#[test]
fn corpus_single_token_deleted() {
    let mut cases = Vec::new();
    for src in samples() {
        let toks = tokens_of(src);
        for i in 0..toks.len() {
            cases.push(splice(src, &toks, i, "", true));
        }
    }
    run_corpus("deletions", cases);
}

#[test]
fn corpus_keyword_injection() {
    let mut cases = Vec::new();
    for src in samples() {
        let toks = tokens_of(src);
        // A spread of positions: every 7th token boundary plus the end.
        let positions: Vec<usize> = (0..=toks.len()).step_by(7).chain([toks.len()]).collect();
        for word in KEYWORDS.iter().chain(PUNCTUATION) {
            for &at in &positions {
                cases.push(splice(src, &toks, at, word, false));
            }
            // Also replace a few tokens outright rather than inserting.
            for at in (3..toks.len()).step_by(23) {
                cases.push(splice(src, &toks, at, word, true));
            }
        }
    }
    run_corpus("keywords", cases);
}

#[test]
fn corpus_random_token_soup() {
    let vocab: Vec<&str> = KEYWORDS
        .iter()
        .chain(PUNCTUATION)
        .chain(SOUP_EXTRAS)
        .copied()
        .collect();
    let mut rng = Lcg(0x5EED_A10F_0014);
    let mut cases = Vec::new();
    for _ in 0..1500 {
        let len = 1 + rng.below(120);
        let mut src = String::new();
        // Half the cases start with a plausible header so the soup reaches
        // deeper parser states than the top-level recovery loop.
        match rng.below(4) {
            0 => src.push_str("package p is type r is record "),
            1 => src.push_str("architecture a of e is begin process begin "),
            2 => src.push_str("entity e is port ( "),
            _ => {}
        }
        for _ in 0..len {
            src.push_str(vocab[rng.below(vocab.len())]);
            src.push(' ');
        }
        cases.push(src);
    }
    run_corpus("soup", cases);
}
