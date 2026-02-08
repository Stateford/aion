//! AST node types for the Verilog-2005 parser.
//!
//! Every AST node carries a [`Span`] for source location tracking.
//! Error recovery is represented by `Error(Span)` variants in
//! [`VerilogItem`], [`ModuleItem`], [`Statement`], and [`Expr`].

use aion_common::Ident;
use aion_source::Span;
use serde::{Deserialize, Serialize};

// ============================================================================
// Top-level
// ============================================================================

/// A complete Verilog source file, containing one or more top-level items.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerilogSourceFile {
    /// The top-level items (modules, etc.) in this file.
    pub items: Vec<VerilogItem>,
    /// The span covering the entire file.
    pub span: Span,
}

/// A top-level item in a Verilog source file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VerilogItem {
    /// A module declaration.
    Module(ModuleDecl),
    /// An error node produced during error recovery.
    Error(Span),
}

// ============================================================================
// Module
// ============================================================================

/// A Verilog module declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleDecl {
    /// The module name.
    pub name: Ident,
    /// Port style: ANSI (declarations in port list) or non-ANSI (names only).
    pub port_style: PortStyle,
    /// Parameter port list (ANSI `#(...)` syntax).
    pub params: Vec<ParameterDecl>,
    /// Port declarations (ANSI-style) or port names (non-ANSI).
    pub ports: Vec<PortDecl>,
    /// Non-ANSI port name references (names listed in module header).
    pub port_names: Vec<Ident>,
    /// Items declared inside the module body.
    pub items: Vec<ModuleItem>,
    /// Source span.
    pub span: Span,
}

/// Whether ports are declared ANSI-style (inline) or non-ANSI (separate).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PortStyle {
    /// ANSI port declarations: `module m(input a, output b);`
    Ansi,
    /// Non-ANSI port list: `module m(a, b);` with separate port declarations.
    NonAnsi,
    /// No ports: `module m;` or `module m();`
    Empty,
}

// ============================================================================
// Ports
// ============================================================================

/// A port declaration (ANSI-style or standalone).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortDecl {
    /// Port direction.
    pub direction: Direction,
    /// Optional net/variable type (`wire`, `reg`, etc.).
    pub net_type: Option<NetType>,
    /// Whether this port is `signed`.
    pub signed: bool,
    /// Optional bit range (e.g., `[7:0]`).
    pub range: Option<Range>,
    /// Port names.
    pub names: Vec<Ident>,
    /// Source span.
    pub span: Span,
}

/// Port or signal direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    /// `input`
    Input,
    /// `output`
    Output,
    /// `inout`
    Inout,
}

/// Net or variable type keyword.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetType {
    /// `wire`
    Wire,
    /// `reg`
    Reg,
    /// `integer`
    Integer,
    /// `real`
    Real,
    /// `tri`
    Tri,
    /// `supply0`
    Supply0,
    /// `supply1`
    Supply1,
}

// ============================================================================
// Parameters
// ============================================================================

/// A parameter or localparam declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterDecl {
    /// Whether this is a `localparam` (true) or `parameter` (false).
    pub local: bool,
    /// Whether this parameter is `signed`.
    pub signed: bool,
    /// Optional bit range.
    pub range: Option<Range>,
    /// Parameter name.
    pub name: Ident,
    /// Default/initial value expression.
    pub value: Option<Expr>,
    /// Source span.
    pub span: Span,
}

// ============================================================================
// Module items
// ============================================================================

/// An item declared inside a module body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ModuleItem {
    /// A net declaration (e.g., `wire [7:0] data;`).
    NetDecl(NetDecl),
    /// A reg declaration (e.g., `reg [7:0] q;`).
    RegDecl(RegDecl),
    /// An integer variable declaration.
    IntegerDecl(IntegerDecl),
    /// A real variable declaration.
    RealDecl(RealDecl),
    /// A parameter declaration.
    ParameterDecl(ParameterDecl),
    /// A localparam declaration.
    LocalparamDecl(ParameterDecl),
    /// A port declaration (non-ANSI style, appearing in module body).
    PortDecl(PortDecl),
    /// A continuous assignment (e.g., `assign y = a & b;`).
    ContinuousAssign(ContinuousAssign),
    /// An `always` block.
    AlwaysBlock(AlwaysBlock),
    /// An `initial` block.
    InitialBlock(InitialBlock),
    /// A module/gate instantiation.
    Instantiation(Instantiation),
    /// A gate primitive instantiation (e.g., `and g1(y, a, b);`).
    GateInst(GateInst),
    /// A `generate` block.
    GenerateBlock(GenerateBlock),
    /// A genvar declaration.
    GenvarDecl(GenvarDecl),
    /// A function declaration.
    FunctionDecl(FunctionDecl),
    /// A task declaration.
    TaskDecl(TaskDecl),
    /// A `defparam` statement.
    DefparamDecl(DefparamDecl),
    /// An error node produced during error recovery.
    Error(Span),
}

// ============================================================================
// Net/Reg/Variable declarations
// ============================================================================

/// A net declaration (e.g., `wire [7:0] data;`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetDecl {
    /// The net type keyword.
    pub net_type: NetType,
    /// Whether this is signed.
    pub signed: bool,
    /// Optional bit range.
    pub range: Option<Range>,
    /// Declared net names, each with optional array dimensions.
    pub names: Vec<DeclName>,
    /// Source span.
    pub span: Span,
}

/// A reg declaration (e.g., `reg [7:0] q;`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegDecl {
    /// Whether this is signed.
    pub signed: bool,
    /// Optional bit range.
    pub range: Option<Range>,
    /// Declared register names with optional array dimensions and initial value.
    pub names: Vec<DeclName>,
    /// Source span.
    pub span: Span,
}

/// An integer variable declaration (e.g., `integer i;`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegerDecl {
    /// Declared names.
    pub names: Vec<DeclName>,
    /// Source span.
    pub span: Span,
}

/// A real variable declaration (e.g., `real x;`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealDecl {
    /// Declared names.
    pub names: Vec<DeclName>,
    /// Source span.
    pub span: Span,
}

/// A declared name with optional array dimensions and initial value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeclName {
    /// The identifier name.
    pub name: Ident,
    /// Optional array dimensions (e.g., `[0:255]`).
    pub dimensions: Vec<Range>,
    /// Optional initial value (for regs).
    pub init: Option<Expr>,
    /// Source span.
    pub span: Span,
}

/// A genvar declaration (e.g., `genvar i;`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenvarDecl {
    /// Declared genvar names.
    pub names: Vec<Ident>,
    /// Source span.
    pub span: Span,
}

/// A defparam statement (e.g., `defparam u1.WIDTH = 16;`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefparamDecl {
    /// The hierarchical parameter name.
    pub target: Expr,
    /// The value expression.
    pub value: Expr,
    /// Source span.
    pub span: Span,
}

// ============================================================================
// Continuous assignment
// ============================================================================

/// A continuous assignment (e.g., `assign y = a & b;`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinuousAssign {
    /// The target net.
    pub target: Expr,
    /// The value expression.
    pub value: Expr,
    /// Source span.
    pub span: Span,
}

// ============================================================================
// Always / Initial blocks
// ============================================================================

/// An `always` block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlwaysBlock {
    /// The body statement (typically an event-controlled block).
    pub body: Statement,
    /// Source span.
    pub span: Span,
}

/// An `initial` block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitialBlock {
    /// The body statement.
    pub body: Statement,
    /// Source span.
    pub span: Span,
}

// ============================================================================
// Instantiation
// ============================================================================

/// A module instantiation (e.g., `counter #(.WIDTH(8)) u1 (.clk(clk));`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instantiation {
    /// The module type name.
    pub module_name: Ident,
    /// Parameter overrides (`#(...)` syntax).
    pub param_overrides: Vec<Connection>,
    /// Instances (name + port connections).
    pub instances: Vec<Instance>,
    /// Source span.
    pub span: Span,
}

/// A single instance within an instantiation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    /// The instance name.
    pub name: Ident,
    /// Optional array range for instance arrays.
    pub range: Option<Range>,
    /// Port connections.
    pub connections: Vec<Connection>,
    /// Source span.
    pub span: Span,
}

/// A port or parameter connection in an instantiation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    /// The formal port/parameter name (if named connection).
    pub formal: Option<Ident>,
    /// The actual expression (may be absent for unconnected ports).
    pub actual: Option<Expr>,
    /// Source span.
    pub span: Span,
}

/// A gate primitive instantiation (e.g., `and g1(y, a, b);`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateInst {
    /// The gate type keyword (e.g., `and`, `or`, `not`).
    pub gate_type: Ident,
    /// The instance name (optional for gates, but we store it).
    pub name: Option<Ident>,
    /// Port connections (positional).
    pub ports: Vec<Expr>,
    /// Source span.
    pub span: Span,
}

// ============================================================================
// Generate
// ============================================================================

/// A generate block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GenerateBlock {
    /// A `for` generate loop.
    For {
        /// The loop variable initialization (e.g., `i = 0`).
        init: Box<Statement>,
        /// The loop condition.
        condition: Expr,
        /// The loop increment (e.g., `i = i + 1`).
        step: Box<Statement>,
        /// Optional block label.
        label: Option<Ident>,
        /// Items in the generate body.
        items: Vec<ModuleItem>,
        /// Source span.
        span: Span,
    },
    /// An `if` generate conditional.
    If {
        /// The condition expression.
        condition: Expr,
        /// Items in the `then` branch.
        then_items: Vec<ModuleItem>,
        /// Items in the `else` branch.
        else_items: Vec<ModuleItem>,
        /// Source span.
        span: Span,
    },
}

// ============================================================================
// Function / Task
// ============================================================================

/// A function declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDecl {
    /// Whether this is `automatic`.
    pub automatic: bool,
    /// Whether the return type is `signed`.
    pub signed: bool,
    /// Optional return type range.
    pub range: Option<Range>,
    /// The function name.
    pub name: Ident,
    /// Input declarations (functions can only have inputs).
    pub inputs: Vec<PortDecl>,
    /// Local declarations inside the function.
    pub decls: Vec<ModuleItem>,
    /// The function body statements.
    pub body: Vec<Statement>,
    /// Source span.
    pub span: Span,
}

/// A task declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDecl {
    /// Whether this is `automatic`.
    pub automatic: bool,
    /// The task name.
    pub name: Ident,
    /// Port declarations.
    pub ports: Vec<PortDecl>,
    /// Local declarations inside the task.
    pub decls: Vec<ModuleItem>,
    /// The task body statements.
    pub body: Vec<Statement>,
    /// Source span.
    pub span: Span,
}

// ============================================================================
// Statements
// ============================================================================

/// A statement in a procedural block (always, initial, function, task).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Statement {
    /// A blocking assignment (e.g., `a = b;`).
    Blocking {
        /// Target expression.
        target: Expr,
        /// Value expression.
        value: Expr,
        /// Source span.
        span: Span,
    },
    /// A non-blocking assignment (e.g., `q <= d;`).
    NonBlocking {
        /// Target expression.
        target: Expr,
        /// Value expression.
        value: Expr,
        /// Source span.
        span: Span,
    },
    /// A `begin ... end` block, optionally labeled.
    Block {
        /// Optional block label.
        label: Option<Ident>,
        /// Block declarations (for named blocks).
        decls: Vec<ModuleItem>,
        /// Statements in the block.
        stmts: Vec<Statement>,
        /// Source span.
        span: Span,
    },
    /// An `if` statement.
    If {
        /// The condition expression.
        condition: Expr,
        /// The `then` branch statement.
        then_stmt: Box<Statement>,
        /// Optional `else` branch statement.
        else_stmt: Option<Box<Statement>>,
        /// Source span.
        span: Span,
    },
    /// A `case`, `casex`, or `casez` statement.
    Case {
        /// The case kind (`case`, `casex`, or `casez`).
        kind: CaseKind,
        /// The expression being matched.
        expr: Expr,
        /// The case arms.
        arms: Vec<CaseArm>,
        /// Source span.
        span: Span,
    },
    /// A `for` loop.
    For {
        /// The initialization statement.
        init: Box<Statement>,
        /// The loop condition.
        condition: Expr,
        /// The increment statement.
        step: Box<Statement>,
        /// The loop body.
        body: Box<Statement>,
        /// Source span.
        span: Span,
    },
    /// A `while` loop.
    While {
        /// The condition expression.
        condition: Expr,
        /// The loop body.
        body: Box<Statement>,
        /// Source span.
        span: Span,
    },
    /// A `forever` loop.
    Forever {
        /// The loop body.
        body: Box<Statement>,
        /// Source span.
        span: Span,
    },
    /// A `repeat` loop.
    Repeat {
        /// Number of repetitions.
        count: Expr,
        /// The loop body.
        body: Box<Statement>,
        /// Source span.
        span: Span,
    },
    /// A `wait` statement.
    Wait {
        /// The condition to wait for.
        condition: Expr,
        /// Optional body statement.
        body: Option<Box<Statement>>,
        /// Source span.
        span: Span,
    },
    /// An event control statement (e.g., `@(posedge clk)`).
    EventControl {
        /// The sensitivity list.
        sensitivity: SensitivityList,
        /// The controlled statement.
        body: Box<Statement>,
        /// Source span.
        span: Span,
    },
    /// A delay control (e.g., `#10 stmt;`).
    Delay {
        /// The delay expression.
        delay: Expr,
        /// The delayed statement.
        body: Box<Statement>,
        /// Source span.
        span: Span,
    },
    /// A task call (e.g., `my_task(a, b);`).
    TaskCall {
        /// The task name expression.
        name: Expr,
        /// Optional arguments.
        args: Vec<Expr>,
        /// Source span.
        span: Span,
    },
    /// A system task call (e.g., `$display("hello");`).
    SystemTaskCall {
        /// The system task name (e.g., `$display`).
        name: Ident,
        /// Arguments.
        args: Vec<Expr>,
        /// Source span.
        span: Span,
    },
    /// A `disable` statement (e.g., `disable block_name;`).
    Disable {
        /// The block or task name to disable.
        name: Ident,
        /// Source span.
        span: Span,
    },
    /// A null statement (lone `;`).
    Null {
        /// Source span.
        span: Span,
    },
    /// An error node produced during error recovery.
    Error(Span),
}

/// The kind of case statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaseKind {
    /// Standard `case` (exact match).
    Case,
    /// `casex` (treats x and z as don't-care in both operands).
    Casex,
    /// `casez` (treats z as don't-care).
    Casez,
}

/// A single arm in a case statement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseArm {
    /// The match expressions, or empty for `default`.
    pub patterns: Vec<Expr>,
    /// Whether this is the `default` arm.
    pub is_default: bool,
    /// The body statement.
    pub body: Statement,
    /// Source span.
    pub span: Span,
}

// ============================================================================
// Sensitivity list
// ============================================================================

/// A sensitivity list for an event control.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SensitivityList {
    /// `@*` or `@(*)` — implicit sensitivity to all read signals.
    Star,
    /// An explicit list of sensitivity items separated by `or` or `,`.
    List(Vec<SensitivityItem>),
}

/// A single item in a sensitivity list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensitivityItem {
    /// Optional edge specifier.
    pub edge: Option<EdgeKind>,
    /// The signal expression.
    pub signal: Expr,
    /// Source span.
    pub span: Span,
}

/// An edge specifier in a sensitivity list or event expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdgeKind {
    /// `posedge`
    Posedge,
    /// `negedge`
    Negedge,
}

// ============================================================================
// Ranges
// ============================================================================

/// A bit range (e.g., `[7:0]`) or array dimension.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Range {
    /// The MSB (left) expression.
    pub msb: Expr,
    /// The LSB (right) expression.
    pub lsb: Expr,
    /// Source span.
    pub span: Span,
}

// ============================================================================
// Expressions
// ============================================================================

/// An expression node in the Verilog AST.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Expr {
    /// A simple identifier.
    Identifier {
        /// The interned identifier.
        name: Ident,
        /// Source span.
        span: Span,
    },
    /// A hierarchical name (e.g., `u1.data`, `top.sub.sig`).
    HierarchicalName {
        /// The parts of the dotted name.
        parts: Vec<Ident>,
        /// Source span.
        span: Span,
    },
    /// A numeric literal (integer, sized, based).
    Literal {
        /// Source span (value extracted from source text).
        span: Span,
    },
    /// A real literal.
    RealLiteral {
        /// Source span.
        span: Span,
    },
    /// A string literal.
    StringLiteral {
        /// Source span.
        span: Span,
    },
    /// A bit/part select (e.g., `data[7]`, `data[7:0]`).
    Index {
        /// The base expression.
        base: Box<Expr>,
        /// The index expression.
        index: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// A range select (e.g., `data[7:0]`).
    RangeSelect {
        /// The base expression.
        base: Box<Expr>,
        /// The MSB expression.
        msb: Box<Expr>,
        /// The LSB expression.
        lsb: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// An indexed part select (e.g., `data[i+:4]` or `data[i-:4]`).
    PartSelect {
        /// The base expression.
        base: Box<Expr>,
        /// The starting index expression.
        index: Box<Expr>,
        /// Whether ascending (`true` for `+:`) or descending (`false` for `-:`).
        ascending: bool,
        /// The width expression.
        width: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// A concatenation (e.g., `{a, b, c}`).
    Concat {
        /// The concatenated expressions.
        elements: Vec<Expr>,
        /// Source span.
        span: Span,
    },
    /// A replication (e.g., `{3{a}}`).
    Repeat {
        /// The repetition count expression.
        count: Box<Expr>,
        /// The concatenation to repeat.
        elements: Vec<Expr>,
        /// Source span.
        span: Span,
    },
    /// A unary operation.
    Unary {
        /// The operator.
        op: UnaryOp,
        /// The operand.
        operand: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// A binary operation.
    Binary {
        /// Left operand.
        left: Box<Expr>,
        /// The operator.
        op: BinaryOp,
        /// Right operand.
        right: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// A ternary conditional expression (e.g., `sel ? a : b`).
    Ternary {
        /// The condition.
        condition: Box<Expr>,
        /// The true-branch expression.
        then_expr: Box<Expr>,
        /// The false-branch expression.
        else_expr: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// A function call (e.g., `clog2(WIDTH)`).
    FuncCall {
        /// The function name.
        name: Box<Expr>,
        /// The arguments.
        args: Vec<Expr>,
        /// Source span.
        span: Span,
    },
    /// A system function call (e.g., `$clog2(WIDTH)`).
    SystemCall {
        /// The system function name (e.g., `$clog2`).
        name: Ident,
        /// The arguments.
        args: Vec<Expr>,
        /// Source span.
        span: Span,
    },
    /// A parenthesized expression.
    Paren {
        /// The inner expression.
        inner: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// An error node produced during error recovery.
    Error(Span),
}

impl Expr {
    /// Returns the source span of this expression.
    pub fn span(&self) -> Span {
        match self {
            Expr::Identifier { span, .. }
            | Expr::HierarchicalName { span, .. }
            | Expr::Literal { span }
            | Expr::RealLiteral { span }
            | Expr::StringLiteral { span }
            | Expr::Index { span, .. }
            | Expr::RangeSelect { span, .. }
            | Expr::PartSelect { span, .. }
            | Expr::Concat { span, .. }
            | Expr::Repeat { span, .. }
            | Expr::Unary { span, .. }
            | Expr::Binary { span, .. }
            | Expr::Ternary { span, .. }
            | Expr::FuncCall { span, .. }
            | Expr::SystemCall { span, .. }
            | Expr::Paren { span, .. }
            | Expr::Error(span) => *span,
        }
    }
}

/// A unary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnaryOp {
    /// `+` (unary plus)
    Plus,
    /// `-` (unary minus)
    Minus,
    /// `!` (logical NOT)
    LogNot,
    /// `~` (bitwise NOT)
    BitNot,
    /// `&` (reduction AND)
    RedAnd,
    /// `~&` (reduction NAND)
    RedNand,
    /// `|` (reduction OR)
    RedOr,
    /// `~|` (reduction NOR)
    RedNor,
    /// `^` (reduction XOR)
    RedXor,
    /// `~^` or `^~` (reduction XNOR)
    RedXnor,
}

/// A binary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BinaryOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Mod,
    /// `**`
    Pow,
    /// `==`
    Eq,
    /// `!=`
    Neq,
    /// `===`
    CaseEq,
    /// `!==`
    CaseNeq,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `&&`
    LogAnd,
    /// `||`
    LogOr,
    /// `&`
    BitAnd,
    /// `|`
    BitOr,
    /// `^`
    BitXor,
    /// `~^` or `^~`
    BitXnor,
    /// `<<`
    Shl,
    /// `>>`
    Shr,
    /// `<<<`
    AShl,
    /// `>>>`
    AShr,
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_source::FileId;

    fn dummy_span() -> Span {
        Span::new(FileId::from_raw(0), 0, 1)
    }

    #[test]
    fn serde_roundtrip_expr() {
        let expr = Expr::Literal { span: dummy_span() };
        let json = serde_json::to_string(&expr).unwrap();
        let back: Expr = serde_json::from_str(&json).unwrap();
        assert_eq!(back.span(), dummy_span());
    }

    #[test]
    fn serde_roundtrip_module() {
        let module = ModuleDecl {
            name: Ident::from_raw(0),
            port_style: PortStyle::Empty,
            params: Vec::new(),
            ports: Vec::new(),
            port_names: Vec::new(),
            items: Vec::new(),
            span: dummy_span(),
        };
        let json = serde_json::to_string(&module).unwrap();
        let back: ModuleDecl = serde_json::from_str(&json).unwrap();
        assert_eq!(back.span, dummy_span());
    }

    #[test]
    fn serde_roundtrip_source_file() {
        let file = VerilogSourceFile {
            items: Vec::new(),
            span: dummy_span(),
        };
        let json = serde_json::to_string(&file).unwrap();
        let back: VerilogSourceFile = serde_json::from_str(&json).unwrap();
        assert!(back.items.is_empty());
    }

    #[test]
    fn serde_roundtrip_statement() {
        let stmt = Statement::Null { span: dummy_span() };
        let json = serde_json::to_string(&stmt).unwrap();
        let back: Statement = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, Statement::Null { .. }));
    }

    #[test]
    fn serde_roundtrip_binary_op() {
        let op = BinaryOp::Add;
        let json = serde_json::to_string(&op).unwrap();
        let back: BinaryOp = serde_json::from_str(&json).unwrap();
        assert_eq!(back, BinaryOp::Add);
    }

    #[test]
    fn serde_roundtrip_case_arm() {
        let arm = CaseArm {
            patterns: Vec::new(),
            is_default: true,
            body: Statement::Null { span: dummy_span() },
            span: dummy_span(),
        };
        let json = serde_json::to_string(&arm).unwrap();
        let back: CaseArm = serde_json::from_str(&json).unwrap();
        assert!(back.is_default);
    }

    #[test]
    fn expr_span_accessor() {
        let span = dummy_span();
        assert_eq!(Expr::Literal { span }.span(), span);
        assert_eq!(Expr::Error(span).span(), span);
        assert_eq!(
            Expr::Identifier {
                name: Ident::from_raw(0),
                span
            }
            .span(),
            span
        );
    }

    #[test]
    fn serde_roundtrip_range() {
        let range = Range {
            msb: Expr::Literal { span: dummy_span() },
            lsb: Expr::Literal { span: dummy_span() },
            span: dummy_span(),
        };
        let json = serde_json::to_string(&range).unwrap();
        let back: Range = serde_json::from_str(&json).unwrap();
        assert_eq!(back.span, dummy_span());
    }
}
