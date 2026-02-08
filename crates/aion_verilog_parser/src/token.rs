//! Token types for the Verilog-2005 lexer.
//!
//! Defines the [`VerilogToken`] enum covering all Verilog-2005 keywords, operators,
//! punctuation, and literals, plus the [`Token`] struct pairing a token kind
//! with its source [`Span`].

use aion_source::Span;
use serde::{Deserialize, Serialize};

/// A Verilog-2005 token kind.
///
/// Keywords are case-sensitive in Verilog — they must appear in lowercase.
/// Literal values are not stored in the token; they are retrieved from the
/// source text using the token's span.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum VerilogToken {
    // === Keywords ===
    /// `always`
    Always,
    /// `and`
    And,
    /// `assign`
    Assign,
    /// `automatic`
    Automatic,
    /// `begin`
    Begin,
    /// `buf`
    Buf,
    /// `case`
    Case,
    /// `casex`
    Casex,
    /// `casez`
    Casez,
    /// `default`
    Default,
    /// `defparam`
    Defparam,
    /// `disable`
    Disable,
    /// `edge`
    Edge,
    /// `else`
    Else,
    /// `end`
    End,
    /// `endcase`
    Endcase,
    /// `endfunction`
    Endfunction,
    /// `endgenerate`
    Endgenerate,
    /// `endmodule`
    Endmodule,
    /// `endtask`
    Endtask,
    /// `for`
    For,
    /// `forever`
    Forever,
    /// `function`
    Function,
    /// `generate`
    Generate,
    /// `genvar`
    Genvar,
    /// `if`
    If,
    /// `initial`
    Initial,
    /// `inout`
    Inout,
    /// `input`
    Input,
    /// `integer`
    Integer,
    /// `localparam`
    Localparam,
    /// `module`
    Module,
    /// `nand`
    Nand,
    /// `negedge`
    Negedge,
    /// `nor`
    Nor,
    /// `not`
    Not,
    /// `or`
    Or,
    /// `output`
    Output,
    /// `parameter`
    Parameter,
    /// `posedge`
    Posedge,
    /// `real`
    Real,
    /// `reg`
    Reg,
    /// `repeat`
    Repeat,
    /// `signed`
    Signed,
    /// `supply0`
    Supply0,
    /// `supply1`
    Supply1,
    /// `task`
    Task,
    /// `tri`
    Tri,
    /// `wait`
    Wait,
    /// `while`
    While,
    /// `wire`
    Wire,
    /// `xnor`
    Xnor,
    /// `xor`
    Xor,

    // === Literals ===
    /// Integer/unsized literal (e.g., `42`, `32'd255`)
    IntLiteral,
    /// Sized/based literal (e.g., `4'b1010`, `16'hFF`, `8'o77`)
    SizedLiteral,
    /// Real literal (e.g., `3.5`, `1.0e-3`)
    RealLiteral,
    /// String literal (e.g., `"hello"`)
    StringLiteral,

    // === Operators and punctuation ===
    /// `(`
    LeftParen,
    /// `)`
    RightParen,
    /// `[`
    LeftBracket,
    /// `]`
    RightBracket,
    /// `{`
    LeftBrace,
    /// `}`
    RightBrace,
    /// `,`
    Comma,
    /// `;`
    Semicolon,
    /// `:`
    Colon,
    /// `.`
    Dot,
    /// `#`
    Hash,
    /// `@`
    At,
    /// `=`
    Equals,
    /// `==`
    DoubleEquals,
    /// `!=`
    BangEquals,
    /// `===`
    TripleEquals,
    /// `!==`
    BangDoubleEquals,
    /// `<`
    LessThan,
    /// `<=`
    LessEquals,
    /// `>`
    GreaterThan,
    /// `>=`
    GreaterEquals,
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `*`
    Star,
    /// `**`
    DoubleStar,
    /// `/`
    Slash,
    /// `%`
    Percent,
    /// `&`
    Ampersand,
    /// `&&`
    DoubleAmpersand,
    /// `|`
    Pipe,
    /// `||`
    DoublePipe,
    /// `^`
    Caret,
    /// `~^` or `^~` (XNOR)
    TildeCaret,
    /// `~`
    Tilde,
    /// `~&` (reduction NAND)
    TildeAmpersand,
    /// `~|` (reduction NOR)
    TildePipe,
    /// `!`
    Bang,
    /// `<<`
    DoubleLess,
    /// `>>`
    DoubleGreater,
    /// `<<<`
    TripleLess,
    /// `>>>`
    TripleGreater,
    /// `?`
    Question,

    // === Identifiers and special ===
    /// A regular identifier (e.g., `my_signal`, `clk`)
    Identifier,
    /// An escaped identifier (e.g., `\my+signal `)
    EscapedIdentifier,
    /// A system identifier (e.g., `$display`, `$clog2`)
    SystemIdentifier,
    /// End of file
    Eof,
    /// Lexer error — unrecognized or malformed token
    Error,
}

impl VerilogToken {
    /// Returns `true` if this token is a keyword.
    pub fn is_keyword(self) -> bool {
        matches!(
            self,
            VerilogToken::Always
                | VerilogToken::And
                | VerilogToken::Assign
                | VerilogToken::Automatic
                | VerilogToken::Begin
                | VerilogToken::Buf
                | VerilogToken::Case
                | VerilogToken::Casex
                | VerilogToken::Casez
                | VerilogToken::Default
                | VerilogToken::Defparam
                | VerilogToken::Disable
                | VerilogToken::Edge
                | VerilogToken::Else
                | VerilogToken::End
                | VerilogToken::Endcase
                | VerilogToken::Endfunction
                | VerilogToken::Endgenerate
                | VerilogToken::Endmodule
                | VerilogToken::Endtask
                | VerilogToken::For
                | VerilogToken::Forever
                | VerilogToken::Function
                | VerilogToken::Generate
                | VerilogToken::Genvar
                | VerilogToken::If
                | VerilogToken::Initial
                | VerilogToken::Inout
                | VerilogToken::Input
                | VerilogToken::Integer
                | VerilogToken::Localparam
                | VerilogToken::Module
                | VerilogToken::Nand
                | VerilogToken::Negedge
                | VerilogToken::Nor
                | VerilogToken::Not
                | VerilogToken::Or
                | VerilogToken::Output
                | VerilogToken::Parameter
                | VerilogToken::Posedge
                | VerilogToken::Real
                | VerilogToken::Reg
                | VerilogToken::Repeat
                | VerilogToken::Signed
                | VerilogToken::Supply0
                | VerilogToken::Supply1
                | VerilogToken::Task
                | VerilogToken::Tri
                | VerilogToken::Wait
                | VerilogToken::While
                | VerilogToken::Wire
                | VerilogToken::Xnor
                | VerilogToken::Xor
        )
    }

    /// Returns `true` if this token is a direction keyword (`input`, `output`, `inout`).
    pub fn is_direction(self) -> bool {
        matches!(
            self,
            VerilogToken::Input | VerilogToken::Output | VerilogToken::Inout
        )
    }

    /// Returns `true` if this token is a net/variable type keyword.
    pub fn is_net_type(self) -> bool {
        matches!(
            self,
            VerilogToken::Wire
                | VerilogToken::Reg
                | VerilogToken::Integer
                | VerilogToken::Real
                | VerilogToken::Tri
                | VerilogToken::Supply0
                | VerilogToken::Supply1
        )
    }
}

/// A lexed token with its kind and source location.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Token {
    /// The kind of this token.
    pub kind: VerilogToken,
    /// The source span covering this token's text.
    pub span: Span,
}

/// Looks up a keyword from an identifier string.
///
/// Returns `Some(VerilogToken)` if the string matches a Verilog-2005 keyword,
/// or `None` if it is a regular identifier. Verilog keywords are case-sensitive.
pub fn lookup_keyword(s: &str) -> Option<VerilogToken> {
    match s {
        "always" => Some(VerilogToken::Always),
        "and" => Some(VerilogToken::And),
        "assign" => Some(VerilogToken::Assign),
        "automatic" => Some(VerilogToken::Automatic),
        "begin" => Some(VerilogToken::Begin),
        "buf" => Some(VerilogToken::Buf),
        "case" => Some(VerilogToken::Case),
        "casex" => Some(VerilogToken::Casex),
        "casez" => Some(VerilogToken::Casez),
        "default" => Some(VerilogToken::Default),
        "defparam" => Some(VerilogToken::Defparam),
        "disable" => Some(VerilogToken::Disable),
        "edge" => Some(VerilogToken::Edge),
        "else" => Some(VerilogToken::Else),
        "end" => Some(VerilogToken::End),
        "endcase" => Some(VerilogToken::Endcase),
        "endfunction" => Some(VerilogToken::Endfunction),
        "endgenerate" => Some(VerilogToken::Endgenerate),
        "endmodule" => Some(VerilogToken::Endmodule),
        "endtask" => Some(VerilogToken::Endtask),
        "for" => Some(VerilogToken::For),
        "forever" => Some(VerilogToken::Forever),
        "function" => Some(VerilogToken::Function),
        "generate" => Some(VerilogToken::Generate),
        "genvar" => Some(VerilogToken::Genvar),
        "if" => Some(VerilogToken::If),
        "initial" => Some(VerilogToken::Initial),
        "inout" => Some(VerilogToken::Inout),
        "input" => Some(VerilogToken::Input),
        "integer" => Some(VerilogToken::Integer),
        "localparam" => Some(VerilogToken::Localparam),
        "module" => Some(VerilogToken::Module),
        "nand" => Some(VerilogToken::Nand),
        "negedge" => Some(VerilogToken::Negedge),
        "nor" => Some(VerilogToken::Nor),
        "not" => Some(VerilogToken::Not),
        "or" => Some(VerilogToken::Or),
        "output" => Some(VerilogToken::Output),
        "parameter" => Some(VerilogToken::Parameter),
        "posedge" => Some(VerilogToken::Posedge),
        "real" => Some(VerilogToken::Real),
        "reg" => Some(VerilogToken::Reg),
        "repeat" => Some(VerilogToken::Repeat),
        "signed" => Some(VerilogToken::Signed),
        "supply0" => Some(VerilogToken::Supply0),
        "supply1" => Some(VerilogToken::Supply1),
        "task" => Some(VerilogToken::Task),
        "tri" => Some(VerilogToken::Tri),
        "wait" => Some(VerilogToken::Wait),
        "while" => Some(VerilogToken::While),
        "wire" => Some(VerilogToken::Wire),
        "xnor" => Some(VerilogToken::Xnor),
        "xor" => Some(VerilogToken::Xor),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_lookup_case_sensitive() {
        assert_eq!(lookup_keyword("module"), Some(VerilogToken::Module));
        assert_eq!(lookup_keyword("Module"), None);
        assert_eq!(lookup_keyword("MODULE"), None);
    }

    #[test]
    fn keyword_lookup_all_keywords() {
        assert_eq!(lookup_keyword("always"), Some(VerilogToken::Always));
        assert_eq!(lookup_keyword("wire"), Some(VerilogToken::Wire));
        assert_eq!(lookup_keyword("endmodule"), Some(VerilogToken::Endmodule));
        assert_eq!(lookup_keyword("posedge"), Some(VerilogToken::Posedge));
        assert_eq!(lookup_keyword("negedge"), Some(VerilogToken::Negedge));
    }

    #[test]
    fn keyword_lookup_non_keyword() {
        assert_eq!(lookup_keyword("my_signal"), None);
        assert_eq!(lookup_keyword("clk"), None);
        assert_eq!(lookup_keyword(""), None);
    }

    #[test]
    fn is_keyword_predicate() {
        assert!(VerilogToken::Module.is_keyword());
        assert!(VerilogToken::Always.is_keyword());
        assert!(!VerilogToken::Identifier.is_keyword());
        assert!(!VerilogToken::Eof.is_keyword());
    }

    #[test]
    fn is_direction_predicate() {
        assert!(VerilogToken::Input.is_direction());
        assert!(VerilogToken::Output.is_direction());
        assert!(VerilogToken::Inout.is_direction());
        assert!(!VerilogToken::Wire.is_direction());
    }

    #[test]
    fn is_net_type_predicate() {
        assert!(VerilogToken::Wire.is_net_type());
        assert!(VerilogToken::Reg.is_net_type());
        assert!(VerilogToken::Integer.is_net_type());
        assert!(!VerilogToken::Module.is_net_type());
    }
}
