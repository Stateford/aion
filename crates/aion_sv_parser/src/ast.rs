//! AST node types for the SystemVerilog-2017 parser.
//!
//! Every AST node carries a `Span` for source location tracking.
//! Error recovery is represented by `Error(Span)` variants in
//! `SvItem`, `ModuleItem`, `Statement`, and `Expr`.
//!
//! Extends the Verilog-2005 AST with SystemVerilog constructs: packages,
//! interfaces, enum/struct/typedef, always_comb/ff/latch, imports, and
//! enhanced expressions/statements.

use aion_common::Ident;
use aion_source::Span;
use serde::{Deserialize, Serialize};

// ============================================================================
// Top-level
// ============================================================================

/// A complete SystemVerilog source file, containing one or more top-level items.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SvSourceFile {
    /// The top-level items (modules, interfaces, packages, etc.) in this file.
    pub items: Vec<SvItem>,
    /// The span covering the entire file.
    pub span: Span,
}

/// A top-level item in a SystemVerilog source file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SvItem {
    /// A module declaration.
    Module(SvModuleDecl),
    /// An interface declaration.
    Interface(SvInterfaceDecl),
    /// A package declaration.
    Package(SvPackageDecl),
    /// An error node produced during error recovery.
    Error(Span),
}

// ============================================================================
// Module
// ============================================================================

/// A SystemVerilog module declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SvModuleDecl {
    /// The module name.
    pub name: Ident,
    /// Port style: ANSI (declarations in port list) or non-ANSI (names only).
    pub port_style: PortStyle,
    /// Parameter port list (ANSI `#(...)` syntax).
    pub params: Vec<ParameterDecl>,
    /// Port declarations (ANSI-style) or port names (non-ANSI).
    pub ports: Vec<SvPortDecl>,
    /// Non-ANSI port name references (names listed in module header).
    pub port_names: Vec<Ident>,
    /// Items declared inside the module body.
    pub items: Vec<ModuleItem>,
    /// Optional end label (e.g., `endmodule : top`).
    pub end_label: Option<Ident>,
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

/// A SystemVerilog port declaration (ANSI-style or standalone).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SvPortDecl {
    /// Port direction.
    pub direction: Direction,
    /// Port type (net, variable, or interface).
    pub port_type: SvPortType,
    /// Whether this port is `signed`.
    pub signed: bool,
    /// Optional bit range (e.g., `[7:0]`).
    pub range: Option<Range>,
    /// Port names.
    pub names: Vec<Ident>,
    /// Source span.
    pub span: Span,
}

/// The type of a SystemVerilog port.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SvPortType {
    /// A net type port (wire, tri, etc.).
    Net(NetType),
    /// A variable type port (logic, bit, reg, etc.).
    Var(VarType),
    /// An interface port (e.g., `axi_if.master`).
    InterfacePort {
        /// The interface type name.
        interface_name: Ident,
        /// Optional modport name.
        modport: Option<Ident>,
    },
    /// No explicit type (inherits from context).
    Implicit,
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

/// Net type keyword.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetType {
    /// `wire`
    Wire,
    /// `tri`
    Tri,
    /// `supply0`
    Supply0,
    /// `supply1`
    Supply1,
}

/// Variable type keyword (SystemVerilog and Verilog).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VarType {
    /// `logic` — 4-state type
    Logic,
    /// `bit` — 2-state type
    Bit,
    /// `byte` — 8-bit 2-state signed
    Byte,
    /// `shortint` — 16-bit 2-state signed
    Shortint,
    /// `int` — 32-bit 2-state signed
    Int,
    /// `longint` — 64-bit 2-state signed
    Longint,
    /// `integer` — 32-bit 4-state signed
    Integer,
    /// `real` — double-precision floating-point
    Real,
    /// `reg` — Verilog-2005 variable type
    Reg,
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
    /// Optional type spec for the parameter (e.g., `int`, `logic [7:0]`).
    pub type_spec: Option<TypeSpec>,
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
// Type specifications
// ============================================================================

/// A type specification used in declarations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TypeSpec {
    /// A simple variable type (logic, bit, int, etc.).
    Simple(VarType),
    /// An enum type.
    Enum(EnumDecl),
    /// A struct type.
    Struct(StructDecl),
    /// A named type (from typedef or type parameter).
    Named(Ident),
    /// A scoped type (e.g., `pkg::type_name`).
    Scoped {
        /// The scope (package or class name).
        scope: Ident,
        /// The type name.
        name: Ident,
    },
}

/// An enum type declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnumDecl {
    /// Optional base type (defaults to int).
    pub base_type: Option<VarType>,
    /// Optional bit range for the base type.
    pub range: Option<Range>,
    /// Enum members.
    pub members: Vec<EnumMember>,
    /// Source span.
    pub span: Span,
}

/// A single member in an enum declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnumMember {
    /// The member name.
    pub name: Ident,
    /// Optional explicit value.
    pub value: Option<Expr>,
    /// Source span.
    pub span: Span,
}

/// A struct (packed) type declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructDecl {
    /// Whether this is `packed`.
    pub packed: bool,
    /// Whether this is `signed` (for packed structs).
    pub signed: bool,
    /// Struct members.
    pub members: Vec<StructMember>,
    /// Source span.
    pub span: Span,
}

/// A single member in a struct declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructMember {
    /// The member type.
    pub type_spec: TypeSpec,
    /// Whether this member is signed.
    pub signed: bool,
    /// Optional bit range.
    pub range: Option<Range>,
    /// Member names.
    pub names: Vec<Ident>,
    /// Source span.
    pub span: Span,
}

/// A typedef declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypedefDecl {
    /// The type being aliased.
    pub type_spec: TypeSpec,
    /// Whether the type is signed.
    pub signed: bool,
    /// Optional bit range.
    pub range: Option<Range>,
    /// The new type name.
    pub name: Ident,
    /// Source span.
    pub span: Span,
}

// ============================================================================
// Module items
// ============================================================================

/// An item declared inside a module, interface, or package body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ModuleItem {
    /// A net declaration (e.g., `wire [7:0] data;`).
    NetDecl(NetDecl),
    /// A reg declaration (e.g., `reg [7:0] q;`).
    RegDecl(RegDecl),
    /// A variable declaration (e.g., `logic [7:0] data;`, `int count;`).
    VarDecl(VarDecl),
    /// An integer variable declaration.
    IntegerDecl(IntegerDecl),
    /// A real variable declaration.
    RealDecl(RealDecl),
    /// A parameter declaration.
    ParameterDecl(ParameterDecl),
    /// A localparam declaration.
    LocalparamDecl(ParameterDecl),
    /// A port declaration (non-ANSI style, appearing in module body).
    PortDecl(SvPortDecl),
    /// A continuous assignment (e.g., `assign y = a & b;`).
    ContinuousAssign(ContinuousAssign),
    /// An `always` block (Verilog-2005 style).
    AlwaysBlock(AlwaysBlock),
    /// An `always_comb` block.
    AlwaysComb(AlwaysCombBlock),
    /// An `always_ff` block.
    AlwaysFf(AlwaysFfBlock),
    /// An `always_latch` block.
    AlwaysLatch(AlwaysLatchBlock),
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
    /// A typedef declaration.
    TypedefDecl(TypedefDecl),
    /// An import statement (e.g., `import pkg::*;`).
    Import(SvImport),
    /// An immediate assertion (assert, assume, cover).
    Assertion(SvAssertion),
    /// A modport declaration (inside an interface).
    ModportDecl(SvModportDecl),
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

/// A SystemVerilog variable declaration (e.g., `logic [7:0] data;`, `int count;`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VarDecl {
    /// The variable type.
    pub var_type: VarType,
    /// Whether this is signed.
    pub signed: bool,
    /// Optional bit range.
    pub range: Option<Range>,
    /// Declared variable names with optional array dimensions and initial value.
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
    /// Optional initial value.
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

/// A Verilog-2005 `always` block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlwaysBlock {
    /// The body statement (typically an event-controlled block).
    pub body: Statement,
    /// Source span.
    pub span: Span,
}

/// An `always_comb` block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlwaysCombBlock {
    /// The body statement.
    pub body: Statement,
    /// Source span.
    pub span: Span,
}

/// An `always_ff` block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlwaysFfBlock {
    /// The sensitivity list (e.g., `@(posedge clk or negedge rst)`).
    pub sensitivity: SensitivityList,
    /// The body statement.
    pub body: Statement,
    /// Source span.
    pub span: Span,
}

/// An `always_latch` block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlwaysLatchBlock {
    /// The body statement.
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
    /// The instance name (optional for gates).
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
    /// Optional return type.
    pub return_type: Option<TypeSpec>,
    /// Whether the return type is `signed`.
    pub signed: bool,
    /// Optional return type range.
    pub range: Option<Range>,
    /// The function name.
    pub name: Ident,
    /// Input declarations.
    pub inputs: Vec<SvPortDecl>,
    /// Local declarations inside the function.
    pub decls: Vec<ModuleItem>,
    /// The function body statements.
    pub body: Vec<Statement>,
    /// Optional end label.
    pub end_label: Option<Ident>,
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
    pub ports: Vec<SvPortDecl>,
    /// Local declarations inside the task.
    pub decls: Vec<ModuleItem>,
    /// The task body statements.
    pub body: Vec<Statement>,
    /// Optional end label.
    pub end_label: Option<Ident>,
    /// Source span.
    pub span: Span,
}

// ============================================================================
// Interface
// ============================================================================

/// A SystemVerilog interface declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SvInterfaceDecl {
    /// The interface name.
    pub name: Ident,
    /// Parameter port list.
    pub params: Vec<ParameterDecl>,
    /// Port declarations.
    pub ports: Vec<SvPortDecl>,
    /// Port style.
    pub port_style: PortStyle,
    /// Items declared inside the interface body.
    pub items: Vec<ModuleItem>,
    /// Optional end label.
    pub end_label: Option<Ident>,
    /// Source span.
    pub span: Span,
}

/// A modport declaration inside an interface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SvModportDecl {
    /// The modport name.
    pub name: Ident,
    /// The ports in this modport.
    pub ports: Vec<SvModportPort>,
    /// Source span.
    pub span: Span,
}

/// A single port within a modport declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SvModportPort {
    /// The port direction.
    pub direction: Direction,
    /// The port names.
    pub names: Vec<Ident>,
    /// Source span.
    pub span: Span,
}

// ============================================================================
// Package
// ============================================================================

/// A SystemVerilog package declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SvPackageDecl {
    /// The package name.
    pub name: Ident,
    /// Items declared inside the package body.
    pub items: Vec<ModuleItem>,
    /// Optional end label.
    pub end_label: Option<Ident>,
    /// Source span.
    pub span: Span,
}

/// An import statement (e.g., `import pkg::*;` or `import pkg::name;`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SvImport {
    /// The package name.
    pub package: Ident,
    /// The imported name, or `None` for wildcard (`*`).
    pub name: Option<Ident>,
    /// Source span.
    pub span: Span,
}

/// An immediate assertion statement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SvAssertion {
    /// The assertion kind.
    pub kind: AssertionKind,
    /// The condition expression.
    pub condition: Expr,
    /// Optional action on pass.
    pub pass_stmt: Option<Box<Statement>>,
    /// Optional action on fail (`else` clause).
    pub fail_stmt: Option<Box<Statement>>,
    /// Source span.
    pub span: Span,
}

/// The kind of immediate assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssertionKind {
    /// `assert`
    Assert,
    /// `assume`
    Assume,
    /// `cover`
    Cover,
}

// ============================================================================
// Statements
// ============================================================================

/// A statement in a procedural block.
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
    /// A compound assignment (e.g., `a += b;`).
    CompoundAssign {
        /// Target expression.
        target: Expr,
        /// The compound operator.
        op: CompoundOp,
        /// Value expression.
        value: Expr,
        /// Source span.
        span: Span,
    },
    /// An increment or decrement (e.g., `i++`, `--j`).
    IncrDecr {
        /// The operand expression.
        operand: Expr,
        /// Whether this is increment (true) or decrement (false).
        increment: bool,
        /// Whether this is prefix (true) or postfix (false).
        prefix: bool,
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
    /// An `if` statement (with optional `unique`/`priority` prefix).
    If {
        /// Unique/priority modifier.
        modifier: Option<CaseModifier>,
        /// The condition expression.
        condition: Expr,
        /// The `then` branch statement.
        then_stmt: Box<Statement>,
        /// Optional `else` branch statement.
        else_stmt: Option<Box<Statement>>,
        /// Source span.
        span: Span,
    },
    /// A `case`, `casex`, or `casez` statement (with optional `unique`/`priority`).
    Case {
        /// Unique/priority modifier.
        modifier: Option<CaseModifier>,
        /// The case kind.
        kind: CaseKind,
        /// The expression being matched.
        expr: Expr,
        /// The case arms.
        arms: Vec<CaseArm>,
        /// Source span.
        span: Span,
    },
    /// A `for` loop (extended with optional `int` variable declaration).
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
    /// A `do ... while` loop.
    DoWhile {
        /// The loop body.
        body: Box<Statement>,
        /// The condition expression.
        condition: Expr,
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
    /// A `foreach` loop (e.g., `foreach (arr[i]) ...`).
    Foreach {
        /// The array expression.
        array: Expr,
        /// The loop variables.
        variables: Vec<Ident>,
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
    /// A `return` statement.
    Return {
        /// Optional return value expression.
        value: Option<Expr>,
        /// Source span.
        span: Span,
    },
    /// A `break` statement.
    Break {
        /// Source span.
        span: Span,
    },
    /// A `continue` statement.
    Continue {
        /// Source span.
        span: Span,
    },
    /// An immediate assertion statement (assert, assume, cover).
    Assertion(SvAssertion),
    /// A variable declaration in a procedural block.
    LocalVarDecl(VarDecl),
    /// A null statement (lone `;`).
    Null {
        /// Source span.
        span: Span,
    },
    /// An error node produced during error recovery.
    Error(Span),
}

/// The compound assignment operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompoundOp {
    /// `+=`
    Add,
    /// `-=`
    Sub,
    /// `*=`
    Mul,
    /// `/=`
    Div,
    /// `%=`
    Mod,
    /// `&=`
    BitAnd,
    /// `|=`
    BitOr,
    /// `^=`
    BitXor,
    /// `<<=`
    Shl,
    /// `>>=`
    Shr,
    /// `<<<=`
    AShl,
    /// `>>>=`
    AShr,
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

/// A modifier for `case` or `if` statements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaseModifier {
    /// `unique` — only one arm must match; no implicit default.
    Unique,
    /// `priority` — first matching arm is selected; no implicit default.
    Priority,
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

/// An expression node in the SystemVerilog AST.
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
    /// A scoped identifier (e.g., `pkg::name`).
    ScopedIdent {
        /// The scope (package or class name).
        scope: Ident,
        /// The identifier within the scope.
        name: Ident,
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
    /// A bit/part select (e.g., `data[7]`).
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
    /// An `inside` expression (e.g., `val inside {1, [3:5]}`).
    Inside {
        /// The value expression.
        expr: Box<Expr>,
        /// The set of range/value patterns.
        ranges: Vec<Expr>,
        /// Source span.
        span: Span,
    },
    /// A type cast expression (e.g., `int'(expr)`, `8'(val)`).
    Cast {
        /// The type or width expression being cast to.
        cast_type: Box<Expr>,
        /// The expression being cast.
        expr: Box<Expr>,
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
            | Expr::ScopedIdent { span, .. }
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
            | Expr::Inside { span, .. }
            | Expr::Cast { span, .. }
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
    /// `++` (prefix increment)
    PreIncr,
    /// `--` (prefix decrement)
    PreDecr,
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
    /// `==?` (wildcard equality)
    WildEq,
    /// `!=?` (wildcard inequality)
    WildNeq,
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
        let module = SvModuleDecl {
            name: Ident::from_raw(0),
            port_style: PortStyle::Empty,
            params: Vec::new(),
            ports: Vec::new(),
            port_names: Vec::new(),
            items: Vec::new(),
            end_label: None,
            span: dummy_span(),
        };
        let json = serde_json::to_string(&module).unwrap();
        let back: SvModuleDecl = serde_json::from_str(&json).unwrap();
        assert_eq!(back.span, dummy_span());
    }

    #[test]
    fn serde_roundtrip_source_file() {
        let file = SvSourceFile {
            items: Vec::new(),
            span: dummy_span(),
        };
        let json = serde_json::to_string(&file).unwrap();
        let back: SvSourceFile = serde_json::from_str(&json).unwrap();
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
    fn serde_roundtrip_sv_import() {
        let import = SvImport {
            package: Ident::from_raw(0),
            name: None,
            span: dummy_span(),
        };
        let json = serde_json::to_string(&import).unwrap();
        let back: SvImport = serde_json::from_str(&json).unwrap();
        assert!(back.name.is_none());
    }

    #[test]
    fn serde_roundtrip_enum_decl() {
        let e = EnumDecl {
            base_type: Some(VarType::Logic),
            range: None,
            members: vec![EnumMember {
                name: Ident::from_raw(0),
                value: None,
                span: dummy_span(),
            }],
            span: dummy_span(),
        };
        let json = serde_json::to_string(&e).unwrap();
        let back: EnumDecl = serde_json::from_str(&json).unwrap();
        assert_eq!(back.members.len(), 1);
    }

    #[test]
    fn serde_roundtrip_compound_op() {
        let op = CompoundOp::Add;
        let json = serde_json::to_string(&op).unwrap();
        let back: CompoundOp = serde_json::from_str(&json).unwrap();
        assert_eq!(back, CompoundOp::Add);
    }

    #[test]
    fn expr_span_accessor() {
        let span = dummy_span();
        assert_eq!(Expr::Literal { span }.span(), span);
        assert_eq!(Expr::Error(span).span(), span);
        assert_eq!(
            Expr::ScopedIdent {
                scope: Ident::from_raw(0),
                name: Ident::from_raw(1),
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
