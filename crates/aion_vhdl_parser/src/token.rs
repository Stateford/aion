//! Token types for the VHDL-2008 lexer.
//!
//! Defines the [`VhdlToken`] enum covering all VHDL-2008 keywords, operators,
//! punctuation, and literals, plus the [`Token`] struct pairing a token kind
//! with its source [`Span`].

use aion_source::Span;
use serde::{Deserialize, Serialize};

/// A VHDL-2008 token kind.
///
/// Keywords are case-insensitive in VHDL — the lexer normalizes them before
/// matching. Literal values are not stored in the token; they are retrieved
/// from the source text using the token's span.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum VhdlToken {
    // === Keywords ===
    /// `abs`
    Abs,
    /// `access`
    Access,
    /// `after`
    After,
    /// `alias`
    Alias,
    /// `all`
    All,
    /// `and`
    And,
    /// `architecture`
    Architecture,
    /// `array`
    Array,
    /// `assert`
    Assert,
    /// `attribute`
    Attribute,
    /// `begin`
    Begin,
    /// `block`
    Block,
    /// `body`
    Body,
    /// `buffer`
    Buffer,
    /// `bus`
    Bus,
    /// `case`
    Case,
    /// `component`
    Component,
    /// `configuration`
    Configuration,
    /// `constant`
    Constant,
    /// `context`
    Context,
    /// `default`
    Default,
    /// `disconnect`
    Disconnect,
    /// `downto`
    Downto,
    /// `else`
    Else,
    /// `elsif`
    Elsif,
    /// `end`
    End,
    /// `entity`
    Entity,
    /// `exit`
    Exit,
    /// `file`
    File,
    /// `for`
    For,
    /// `force`
    Force,
    /// `function`
    Function,
    /// `generate`
    Generate,
    /// `generic`
    Generic,
    /// `group`
    Group,
    /// `guarded`
    Guarded,
    /// `if`
    If,
    /// `impure`
    Impure,
    /// `in`
    In,
    /// `inertial`
    Inertial,
    /// `inout`
    Inout,
    /// `is`
    Is,
    /// `label`
    LabelKw,
    /// `library`
    Library,
    /// `linkage`
    Linkage,
    /// `literal`
    Literal,
    /// `loop`
    Loop,
    /// `map`
    Map,
    /// `mod`
    Mod,
    /// `nand`
    Nand,
    /// `new`
    New,
    /// `next`
    Next,
    /// `nor`
    Nor,
    /// `not`
    Not,
    /// `null`
    Null,
    /// `of`
    Of,
    /// `on`
    On,
    /// `open`
    Open,
    /// `or`
    Or,
    /// `others`
    Others,
    /// `out`
    Out,
    /// `package`
    Package,
    /// `parameter`
    Parameter,
    /// `port`
    Port,
    /// `postponed`
    Postponed,
    /// `procedure`
    Procedure,
    /// `process`
    Process,
    /// `protected`
    Protected,
    /// `pure`
    Pure,
    /// `range`
    Range,
    /// `record`
    Record,
    /// `register`
    Register,
    /// `reject`
    Reject,
    /// `release`
    Release,
    /// `rem`
    Rem,
    /// `report`
    Report,
    /// `return`
    Return,
    /// `rol`
    Rol,
    /// `ror`
    Ror,
    /// `select`
    Select,
    /// `severity`
    Severity,
    /// `signal`
    Signal,
    /// `shared`
    Shared,
    /// `sla`
    Sla,
    /// `sll`
    Sll,
    /// `sra`
    Sra,
    /// `srl`
    Srl,
    /// `subtype`
    Subtype,
    /// `then`
    Then,
    /// `to`
    To,
    /// `transport`
    Transport,
    /// `type`
    Type,
    /// `unaffected`
    Unaffected,
    /// `units`
    Units,
    /// `until`
    Until,
    /// `use`
    Use,
    /// `variable`
    Variable,
    /// `wait`
    Wait,
    /// `when`
    When,
    /// `while`
    While,
    /// `with`
    With,
    /// `xnor`
    Xnor,
    /// `xor`
    Xor,

    // === Literals ===
    /// Integer literal (e.g., `42`, `16#FF#`)
    IntLiteral,
    /// Real literal (e.g., `3.5`, `1.0e-3`)
    RealLiteral,
    /// Character literal (e.g., `'0'`, `'Z'`)
    CharLiteral,
    /// String literal (e.g., `"hello"`)
    StringLiteral,
    /// Bit string literal (e.g., `X"FF"`, `B"1010"`, `O"77"`)
    BitStringLiteral,

    // === Operators and punctuation ===
    /// `(`
    LeftParen,
    /// `)`
    RightParen,
    /// `,`
    Comma,
    /// `;`
    Semicolon,
    /// `:`
    Colon,
    /// `:=`
    ColonEquals,
    /// `<=`  (signal assignment or relational operator — disambiguated by parser)
    LessEquals,
    /// `=>`
    Arrow,
    /// `&`
    Ampersand,
    /// `*`
    Star,
    /// `**`
    DoubleStar,
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `/`
    Slash,
    /// `=`
    Equals,
    /// `/=`
    SlashEquals,
    /// `<`
    LessThan,
    /// `>`
    GreaterThan,
    /// `>=`
    GreaterEquals,
    /// `|`
    Bar,
    /// `'` (tick — attribute access)
    Tick,
    /// `.`
    Dot,
    /// `<<`
    DoubleLess,
    /// `>>`
    DoubleGreater,
    /// `?=`
    MatchEquals,
    /// `?/=`
    MatchSlashEquals,
    /// `?<`
    MatchLess,
    /// `?<=`
    MatchLessEquals,
    /// `?>`
    MatchGreater,
    /// `?>=`
    MatchGreaterEquals,
    /// `??`
    ConditionOp,
    /// `^`
    Caret,
    /// `@`
    At,

    // === Identifiers and special ===
    /// A regular identifier (e.g., `my_signal`, `CLK`)
    Identifier,
    /// An extended identifier (e.g., `\my signal\`)
    ExtendedIdentifier,
    /// End of file
    Eof,
    /// Lexer error — unrecognized or malformed token
    Error,
}

impl VhdlToken {
    /// Returns `true` if this token is a keyword.
    pub fn is_keyword(self) -> bool {
        matches!(
            self,
            VhdlToken::Abs
                | VhdlToken::Access
                | VhdlToken::After
                | VhdlToken::Alias
                | VhdlToken::All
                | VhdlToken::And
                | VhdlToken::Architecture
                | VhdlToken::Array
                | VhdlToken::Assert
                | VhdlToken::Attribute
                | VhdlToken::Begin
                | VhdlToken::Block
                | VhdlToken::Body
                | VhdlToken::Buffer
                | VhdlToken::Bus
                | VhdlToken::Case
                | VhdlToken::Component
                | VhdlToken::Configuration
                | VhdlToken::Constant
                | VhdlToken::Context
                | VhdlToken::Default
                | VhdlToken::Disconnect
                | VhdlToken::Downto
                | VhdlToken::Else
                | VhdlToken::Elsif
                | VhdlToken::End
                | VhdlToken::Entity
                | VhdlToken::Exit
                | VhdlToken::File
                | VhdlToken::For
                | VhdlToken::Force
                | VhdlToken::Function
                | VhdlToken::Generate
                | VhdlToken::Generic
                | VhdlToken::Group
                | VhdlToken::Guarded
                | VhdlToken::If
                | VhdlToken::Impure
                | VhdlToken::In
                | VhdlToken::Inertial
                | VhdlToken::Inout
                | VhdlToken::Is
                | VhdlToken::LabelKw
                | VhdlToken::Library
                | VhdlToken::Linkage
                | VhdlToken::Literal
                | VhdlToken::Loop
                | VhdlToken::Map
                | VhdlToken::Mod
                | VhdlToken::Nand
                | VhdlToken::New
                | VhdlToken::Next
                | VhdlToken::Nor
                | VhdlToken::Not
                | VhdlToken::Null
                | VhdlToken::Of
                | VhdlToken::On
                | VhdlToken::Open
                | VhdlToken::Or
                | VhdlToken::Others
                | VhdlToken::Out
                | VhdlToken::Package
                | VhdlToken::Parameter
                | VhdlToken::Port
                | VhdlToken::Postponed
                | VhdlToken::Procedure
                | VhdlToken::Process
                | VhdlToken::Protected
                | VhdlToken::Pure
                | VhdlToken::Range
                | VhdlToken::Record
                | VhdlToken::Register
                | VhdlToken::Reject
                | VhdlToken::Release
                | VhdlToken::Rem
                | VhdlToken::Report
                | VhdlToken::Return
                | VhdlToken::Rol
                | VhdlToken::Ror
                | VhdlToken::Select
                | VhdlToken::Severity
                | VhdlToken::Signal
                | VhdlToken::Shared
                | VhdlToken::Sla
                | VhdlToken::Sll
                | VhdlToken::Sra
                | VhdlToken::Srl
                | VhdlToken::Subtype
                | VhdlToken::Then
                | VhdlToken::To
                | VhdlToken::Transport
                | VhdlToken::Type
                | VhdlToken::Unaffected
                | VhdlToken::Units
                | VhdlToken::Until
                | VhdlToken::Use
                | VhdlToken::Variable
                | VhdlToken::Wait
                | VhdlToken::When
                | VhdlToken::While
                | VhdlToken::With
                | VhdlToken::Xnor
                | VhdlToken::Xor
        )
    }
}

/// A lexed token with its kind and source location.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Token {
    /// The kind of this token.
    pub kind: VhdlToken,
    /// The source span covering this token's text.
    pub span: Span,
}

/// Looks up a keyword from a lowercase identifier string.
///
/// Returns `Some(VhdlToken)` if the string matches a VHDL-2008 keyword,
/// or `None` if it is a regular identifier.
pub fn lookup_keyword(s: &str) -> Option<VhdlToken> {
    match s {
        "abs" => Some(VhdlToken::Abs),
        "access" => Some(VhdlToken::Access),
        "after" => Some(VhdlToken::After),
        "alias" => Some(VhdlToken::Alias),
        "all" => Some(VhdlToken::All),
        "and" => Some(VhdlToken::And),
        "architecture" => Some(VhdlToken::Architecture),
        "array" => Some(VhdlToken::Array),
        "assert" => Some(VhdlToken::Assert),
        "attribute" => Some(VhdlToken::Attribute),
        "begin" => Some(VhdlToken::Begin),
        "block" => Some(VhdlToken::Block),
        "body" => Some(VhdlToken::Body),
        "buffer" => Some(VhdlToken::Buffer),
        "bus" => Some(VhdlToken::Bus),
        "case" => Some(VhdlToken::Case),
        "component" => Some(VhdlToken::Component),
        "configuration" => Some(VhdlToken::Configuration),
        "constant" => Some(VhdlToken::Constant),
        "context" => Some(VhdlToken::Context),
        "default" => Some(VhdlToken::Default),
        "disconnect" => Some(VhdlToken::Disconnect),
        "downto" => Some(VhdlToken::Downto),
        "else" => Some(VhdlToken::Else),
        "elsif" => Some(VhdlToken::Elsif),
        "end" => Some(VhdlToken::End),
        "entity" => Some(VhdlToken::Entity),
        "exit" => Some(VhdlToken::Exit),
        "file" => Some(VhdlToken::File),
        "for" => Some(VhdlToken::For),
        "force" => Some(VhdlToken::Force),
        "function" => Some(VhdlToken::Function),
        "generate" => Some(VhdlToken::Generate),
        "generic" => Some(VhdlToken::Generic),
        "group" => Some(VhdlToken::Group),
        "guarded" => Some(VhdlToken::Guarded),
        "if" => Some(VhdlToken::If),
        "impure" => Some(VhdlToken::Impure),
        "in" => Some(VhdlToken::In),
        "inertial" => Some(VhdlToken::Inertial),
        "inout" => Some(VhdlToken::Inout),
        "is" => Some(VhdlToken::Is),
        "label" => Some(VhdlToken::LabelKw),
        "library" => Some(VhdlToken::Library),
        "linkage" => Some(VhdlToken::Linkage),
        "literal" => Some(VhdlToken::Literal),
        "loop" => Some(VhdlToken::Loop),
        "map" => Some(VhdlToken::Map),
        "mod" => Some(VhdlToken::Mod),
        "nand" => Some(VhdlToken::Nand),
        "new" => Some(VhdlToken::New),
        "next" => Some(VhdlToken::Next),
        "nor" => Some(VhdlToken::Nor),
        "not" => Some(VhdlToken::Not),
        "null" => Some(VhdlToken::Null),
        "of" => Some(VhdlToken::Of),
        "on" => Some(VhdlToken::On),
        "open" => Some(VhdlToken::Open),
        "or" => Some(VhdlToken::Or),
        "others" => Some(VhdlToken::Others),
        "out" => Some(VhdlToken::Out),
        "package" => Some(VhdlToken::Package),
        "parameter" => Some(VhdlToken::Parameter),
        "port" => Some(VhdlToken::Port),
        "postponed" => Some(VhdlToken::Postponed),
        "procedure" => Some(VhdlToken::Procedure),
        "process" => Some(VhdlToken::Process),
        "protected" => Some(VhdlToken::Protected),
        "pure" => Some(VhdlToken::Pure),
        "range" => Some(VhdlToken::Range),
        "record" => Some(VhdlToken::Record),
        "register" => Some(VhdlToken::Register),
        "reject" => Some(VhdlToken::Reject),
        "release" => Some(VhdlToken::Release),
        "rem" => Some(VhdlToken::Rem),
        "report" => Some(VhdlToken::Report),
        "return" => Some(VhdlToken::Return),
        "rol" => Some(VhdlToken::Rol),
        "ror" => Some(VhdlToken::Ror),
        "select" => Some(VhdlToken::Select),
        "severity" => Some(VhdlToken::Severity),
        "signal" => Some(VhdlToken::Signal),
        "shared" => Some(VhdlToken::Shared),
        "sla" => Some(VhdlToken::Sla),
        "sll" => Some(VhdlToken::Sll),
        "sra" => Some(VhdlToken::Sra),
        "srl" => Some(VhdlToken::Srl),
        "subtype" => Some(VhdlToken::Subtype),
        "then" => Some(VhdlToken::Then),
        "to" => Some(VhdlToken::To),
        "transport" => Some(VhdlToken::Transport),
        "type" => Some(VhdlToken::Type),
        "unaffected" => Some(VhdlToken::Unaffected),
        "units" => Some(VhdlToken::Units),
        "until" => Some(VhdlToken::Until),
        "use" => Some(VhdlToken::Use),
        "variable" => Some(VhdlToken::Variable),
        "wait" => Some(VhdlToken::Wait),
        "when" => Some(VhdlToken::When),
        "while" => Some(VhdlToken::While),
        "with" => Some(VhdlToken::With),
        "xnor" => Some(VhdlToken::Xnor),
        "xor" => Some(VhdlToken::Xor),
        _ => None,
    }
}
