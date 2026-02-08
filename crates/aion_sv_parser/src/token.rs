//! Token types for the SystemVerilog-2017 lexer.
//!
//! Defines the `SvToken` enum covering all Verilog-2005 keywords, SystemVerilog
//! extensions (logic, bit, enum, struct, interface, package, always_comb, etc.),
//! operators, punctuation, and literals, plus the `Token` struct pairing a token
//! kind with its source `Span`.

use aion_source::Span;
use serde::{Deserialize, Serialize};

/// A SystemVerilog-2017 token kind.
///
/// Keywords are case-sensitive — they must appear in lowercase.
/// Literal values are not stored in the token; they are retrieved from the
/// source text using the token's span.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum SvToken {
    // === Verilog-2005 Keywords ===
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
    /// `unsigned`
    Unsigned,
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

    // === SystemVerilog Keywords ===
    /// `always_comb`
    AlwaysComb,
    /// `always_ff`
    AlwaysFf,
    /// `always_latch`
    AlwaysLatch,
    /// `assert`
    Assert,
    /// `assume`
    Assume,
    /// `bit`
    Bit,
    /// `break`
    Break,
    /// `byte`
    Byte,
    /// `const`
    Const,
    /// `continue`
    Continue,
    /// `cover`
    Cover,
    /// `do`
    Do,
    /// `endinterface`
    Endinterface,
    /// `endpackage`
    Endpackage,
    /// `enum`
    Enum,
    /// `export`
    Export,
    /// `foreach`
    Foreach,
    /// `import`
    Import,
    /// `inside`
    Inside,
    /// `int`
    Int,
    /// `interface`
    Interface,
    /// `logic`
    Logic,
    /// `longint`
    Longint,
    /// `modport`
    Modport,
    /// `package`
    Package,
    /// `packed`
    Packed,
    /// `priority`
    Priority,
    /// `return`
    Return,
    /// `shortint`
    Shortint,
    /// `static`
    Static,
    /// `struct`
    Struct,
    /// `typedef`
    Typedef,
    /// `union`
    Union,
    /// `unique`
    Unique,
    /// `var`
    Var,
    /// `void`
    Void,

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
    /// `==?` (wildcard equality)
    WildcardEq,
    /// `!=?` (wildcard inequality)
    WildcardNeq,
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
    /// `++`
    PlusPlus,
    /// `--`
    MinusMinus,
    /// `+=`
    PlusEquals,
    /// `-=`
    MinusEquals,
    /// `*=`
    StarEquals,
    /// `/=`
    SlashEquals,
    /// `%=`
    PercentEquals,
    /// `&=`
    AmpersandEquals,
    /// `|=`
    PipeEquals,
    /// `^=`
    CaretEquals,
    /// `<<=`
    DoubleLessEquals,
    /// `>>=`
    DoubleGreaterEquals,
    /// `<<<=`
    TripleLessEquals,
    /// `>>>=`
    TripleGreaterEquals,
    /// `::` (scope resolution)
    ColonColon,
    /// `->` (event trigger)
    Arrow,
    /// `'` (tick, used for casts like `type'(expr)`)
    Tick,

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

impl SvToken {
    /// Returns `true` if this token is a keyword.
    pub fn is_keyword(self) -> bool {
        matches!(
            self,
            SvToken::Always
                | SvToken::And
                | SvToken::Assign
                | SvToken::Automatic
                | SvToken::Begin
                | SvToken::Buf
                | SvToken::Case
                | SvToken::Casex
                | SvToken::Casez
                | SvToken::Default
                | SvToken::Defparam
                | SvToken::Disable
                | SvToken::Edge
                | SvToken::Else
                | SvToken::End
                | SvToken::Endcase
                | SvToken::Endfunction
                | SvToken::Endgenerate
                | SvToken::Endmodule
                | SvToken::Endtask
                | SvToken::For
                | SvToken::Forever
                | SvToken::Function
                | SvToken::Generate
                | SvToken::Genvar
                | SvToken::If
                | SvToken::Initial
                | SvToken::Inout
                | SvToken::Input
                | SvToken::Integer
                | SvToken::Localparam
                | SvToken::Module
                | SvToken::Nand
                | SvToken::Negedge
                | SvToken::Nor
                | SvToken::Not
                | SvToken::Or
                | SvToken::Output
                | SvToken::Parameter
                | SvToken::Posedge
                | SvToken::Real
                | SvToken::Reg
                | SvToken::Repeat
                | SvToken::Signed
                | SvToken::Supply0
                | SvToken::Supply1
                | SvToken::Task
                | SvToken::Tri
                | SvToken::Unsigned
                | SvToken::Wait
                | SvToken::While
                | SvToken::Wire
                | SvToken::Xnor
                | SvToken::Xor
                // SV keywords
                | SvToken::AlwaysComb
                | SvToken::AlwaysFf
                | SvToken::AlwaysLatch
                | SvToken::Assert
                | SvToken::Assume
                | SvToken::Bit
                | SvToken::Break
                | SvToken::Byte
                | SvToken::Const
                | SvToken::Continue
                | SvToken::Cover
                | SvToken::Do
                | SvToken::Endinterface
                | SvToken::Endpackage
                | SvToken::Enum
                | SvToken::Export
                | SvToken::Foreach
                | SvToken::Import
                | SvToken::Inside
                | SvToken::Int
                | SvToken::Interface
                | SvToken::Logic
                | SvToken::Longint
                | SvToken::Modport
                | SvToken::Package
                | SvToken::Packed
                | SvToken::Priority
                | SvToken::Return
                | SvToken::Shortint
                | SvToken::Static
                | SvToken::Struct
                | SvToken::Typedef
                | SvToken::Union
                | SvToken::Unique
                | SvToken::Var
                | SvToken::Void
        )
    }

    /// Returns `true` if this token is a direction keyword (`input`, `output`, `inout`).
    pub fn is_direction(self) -> bool {
        matches!(self, SvToken::Input | SvToken::Output | SvToken::Inout)
    }

    /// Returns `true` if this token is a net/variable type keyword.
    pub fn is_net_type(self) -> bool {
        matches!(
            self,
            SvToken::Wire
                | SvToken::Reg
                | SvToken::Integer
                | SvToken::Real
                | SvToken::Tri
                | SvToken::Supply0
                | SvToken::Supply1
        )
    }

    /// Returns `true` if this token is a SystemVerilog data type keyword.
    pub fn is_data_type(self) -> bool {
        matches!(
            self,
            SvToken::Logic
                | SvToken::Bit
                | SvToken::Byte
                | SvToken::Shortint
                | SvToken::Int
                | SvToken::Longint
                | SvToken::Integer
                | SvToken::Real
                | SvToken::Reg
                | SvToken::Wire
        )
    }

    /// Returns `true` if this token is an always-block variant keyword.
    pub fn is_always_variant(self) -> bool {
        matches!(
            self,
            SvToken::Always | SvToken::AlwaysComb | SvToken::AlwaysFf | SvToken::AlwaysLatch
        )
    }

    /// Returns `true` if this token is a compound assignment operator.
    pub fn is_assignment_op(self) -> bool {
        matches!(
            self,
            SvToken::PlusEquals
                | SvToken::MinusEquals
                | SvToken::StarEquals
                | SvToken::SlashEquals
                | SvToken::PercentEquals
                | SvToken::AmpersandEquals
                | SvToken::PipeEquals
                | SvToken::CaretEquals
                | SvToken::DoubleLessEquals
                | SvToken::DoubleGreaterEquals
                | SvToken::TripleLessEquals
                | SvToken::TripleGreaterEquals
        )
    }
}

/// A lexed token with its kind and source location.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Token {
    /// The kind of this token.
    pub kind: SvToken,
    /// The source span covering this token's text.
    pub span: Span,
}

/// Looks up a keyword from an identifier string.
///
/// Returns `Some(SvToken)` if the string matches a Verilog-2005 or SystemVerilog
/// keyword, or `None` if it is a regular identifier. Keywords are case-sensitive.
pub fn lookup_keyword(s: &str) -> Option<SvToken> {
    match s {
        // Verilog-2005 keywords
        "always" => Some(SvToken::Always),
        "and" => Some(SvToken::And),
        "assign" => Some(SvToken::Assign),
        "automatic" => Some(SvToken::Automatic),
        "begin" => Some(SvToken::Begin),
        "buf" => Some(SvToken::Buf),
        "case" => Some(SvToken::Case),
        "casex" => Some(SvToken::Casex),
        "casez" => Some(SvToken::Casez),
        "default" => Some(SvToken::Default),
        "defparam" => Some(SvToken::Defparam),
        "disable" => Some(SvToken::Disable),
        "edge" => Some(SvToken::Edge),
        "else" => Some(SvToken::Else),
        "end" => Some(SvToken::End),
        "endcase" => Some(SvToken::Endcase),
        "endfunction" => Some(SvToken::Endfunction),
        "endgenerate" => Some(SvToken::Endgenerate),
        "endmodule" => Some(SvToken::Endmodule),
        "endtask" => Some(SvToken::Endtask),
        "for" => Some(SvToken::For),
        "forever" => Some(SvToken::Forever),
        "function" => Some(SvToken::Function),
        "generate" => Some(SvToken::Generate),
        "genvar" => Some(SvToken::Genvar),
        "if" => Some(SvToken::If),
        "initial" => Some(SvToken::Initial),
        "inout" => Some(SvToken::Inout),
        "input" => Some(SvToken::Input),
        "integer" => Some(SvToken::Integer),
        "localparam" => Some(SvToken::Localparam),
        "module" => Some(SvToken::Module),
        "nand" => Some(SvToken::Nand),
        "negedge" => Some(SvToken::Negedge),
        "nor" => Some(SvToken::Nor),
        "not" => Some(SvToken::Not),
        "or" => Some(SvToken::Or),
        "output" => Some(SvToken::Output),
        "parameter" => Some(SvToken::Parameter),
        "posedge" => Some(SvToken::Posedge),
        "real" => Some(SvToken::Real),
        "reg" => Some(SvToken::Reg),
        "repeat" => Some(SvToken::Repeat),
        "signed" => Some(SvToken::Signed),
        "supply0" => Some(SvToken::Supply0),
        "supply1" => Some(SvToken::Supply1),
        "task" => Some(SvToken::Task),
        "tri" => Some(SvToken::Tri),
        "unsigned" => Some(SvToken::Unsigned),
        "wait" => Some(SvToken::Wait),
        "while" => Some(SvToken::While),
        "wire" => Some(SvToken::Wire),
        "xnor" => Some(SvToken::Xnor),
        "xor" => Some(SvToken::Xor),
        // SystemVerilog keywords
        "always_comb" => Some(SvToken::AlwaysComb),
        "always_ff" => Some(SvToken::AlwaysFf),
        "always_latch" => Some(SvToken::AlwaysLatch),
        "assert" => Some(SvToken::Assert),
        "assume" => Some(SvToken::Assume),
        "bit" => Some(SvToken::Bit),
        "break" => Some(SvToken::Break),
        "byte" => Some(SvToken::Byte),
        "const" => Some(SvToken::Const),
        "continue" => Some(SvToken::Continue),
        "cover" => Some(SvToken::Cover),
        "do" => Some(SvToken::Do),
        "endinterface" => Some(SvToken::Endinterface),
        "endpackage" => Some(SvToken::Endpackage),
        "enum" => Some(SvToken::Enum),
        "export" => Some(SvToken::Export),
        "foreach" => Some(SvToken::Foreach),
        "import" => Some(SvToken::Import),
        "inside" => Some(SvToken::Inside),
        "int" => Some(SvToken::Int),
        "interface" => Some(SvToken::Interface),
        "logic" => Some(SvToken::Logic),
        "longint" => Some(SvToken::Longint),
        "modport" => Some(SvToken::Modport),
        "package" => Some(SvToken::Package),
        "packed" => Some(SvToken::Packed),
        "priority" => Some(SvToken::Priority),
        "return" => Some(SvToken::Return),
        "shortint" => Some(SvToken::Shortint),
        "static" => Some(SvToken::Static),
        "struct" => Some(SvToken::Struct),
        "typedef" => Some(SvToken::Typedef),
        "union" => Some(SvToken::Union),
        "unique" => Some(SvToken::Unique),
        "var" => Some(SvToken::Var),
        "void" => Some(SvToken::Void),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_lookup_case_sensitive() {
        assert_eq!(lookup_keyword("module"), Some(SvToken::Module));
        assert_eq!(lookup_keyword("Module"), None);
        assert_eq!(lookup_keyword("MODULE"), None);
    }

    #[test]
    fn keyword_lookup_verilog_keywords() {
        assert_eq!(lookup_keyword("always"), Some(SvToken::Always));
        assert_eq!(lookup_keyword("wire"), Some(SvToken::Wire));
        assert_eq!(lookup_keyword("endmodule"), Some(SvToken::Endmodule));
        assert_eq!(lookup_keyword("posedge"), Some(SvToken::Posedge));
        assert_eq!(lookup_keyword("negedge"), Some(SvToken::Negedge));
    }

    #[test]
    fn keyword_lookup_sv_keywords() {
        assert_eq!(lookup_keyword("always_comb"), Some(SvToken::AlwaysComb));
        assert_eq!(lookup_keyword("always_ff"), Some(SvToken::AlwaysFf));
        assert_eq!(lookup_keyword("always_latch"), Some(SvToken::AlwaysLatch));
        assert_eq!(lookup_keyword("logic"), Some(SvToken::Logic));
        assert_eq!(lookup_keyword("bit"), Some(SvToken::Bit));
        assert_eq!(lookup_keyword("byte"), Some(SvToken::Byte));
        assert_eq!(lookup_keyword("int"), Some(SvToken::Int));
        assert_eq!(lookup_keyword("longint"), Some(SvToken::Longint));
        assert_eq!(lookup_keyword("enum"), Some(SvToken::Enum));
        assert_eq!(lookup_keyword("struct"), Some(SvToken::Struct));
        assert_eq!(lookup_keyword("typedef"), Some(SvToken::Typedef));
        assert_eq!(lookup_keyword("interface"), Some(SvToken::Interface));
        assert_eq!(lookup_keyword("package"), Some(SvToken::Package));
        assert_eq!(lookup_keyword("import"), Some(SvToken::Import));
        assert_eq!(lookup_keyword("modport"), Some(SvToken::Modport));
        assert_eq!(lookup_keyword("unique"), Some(SvToken::Unique));
        assert_eq!(lookup_keyword("priority"), Some(SvToken::Priority));
        assert_eq!(lookup_keyword("inside"), Some(SvToken::Inside));
    }

    #[test]
    fn keyword_lookup_non_keyword() {
        assert_eq!(lookup_keyword("my_signal"), None);
        assert_eq!(lookup_keyword("clk"), None);
        assert_eq!(lookup_keyword(""), None);
    }

    #[test]
    fn is_keyword_predicate() {
        assert!(SvToken::Module.is_keyword());
        assert!(SvToken::Always.is_keyword());
        assert!(SvToken::Logic.is_keyword());
        assert!(SvToken::AlwaysComb.is_keyword());
        assert!(!SvToken::Identifier.is_keyword());
        assert!(!SvToken::Eof.is_keyword());
    }

    #[test]
    fn is_direction_predicate() {
        assert!(SvToken::Input.is_direction());
        assert!(SvToken::Output.is_direction());
        assert!(SvToken::Inout.is_direction());
        assert!(!SvToken::Wire.is_direction());
    }

    #[test]
    fn is_net_type_predicate() {
        assert!(SvToken::Wire.is_net_type());
        assert!(SvToken::Reg.is_net_type());
        assert!(SvToken::Integer.is_net_type());
        assert!(!SvToken::Module.is_net_type());
    }

    #[test]
    fn is_data_type_predicate() {
        assert!(SvToken::Logic.is_data_type());
        assert!(SvToken::Bit.is_data_type());
        assert!(SvToken::Byte.is_data_type());
        assert!(SvToken::Int.is_data_type());
        assert!(SvToken::Longint.is_data_type());
        assert!(!SvToken::Module.is_data_type());
    }

    #[test]
    fn is_always_variant_predicate() {
        assert!(SvToken::Always.is_always_variant());
        assert!(SvToken::AlwaysComb.is_always_variant());
        assert!(SvToken::AlwaysFf.is_always_variant());
        assert!(SvToken::AlwaysLatch.is_always_variant());
        assert!(!SvToken::Initial.is_always_variant());
    }

    #[test]
    fn is_assignment_op_predicate() {
        assert!(SvToken::PlusEquals.is_assignment_op());
        assert!(SvToken::MinusEquals.is_assignment_op());
        assert!(SvToken::StarEquals.is_assignment_op());
        assert!(SvToken::AmpersandEquals.is_assignment_op());
        assert!(!SvToken::Equals.is_assignment_op());
        assert!(!SvToken::Plus.is_assignment_op());
    }
}
