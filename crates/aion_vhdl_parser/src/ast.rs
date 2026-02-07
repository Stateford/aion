//! AST node types for the VHDL-2008 parser.
//!
//! Every AST node carries a [`Span`] for source location tracking.
//! Error recovery is represented by `Error(Span)` variants in
//! [`DesignUnit`], [`Declaration`], [`ConcurrentStatement`],
//! [`SequentialStatement`], and [`Expr`].

use aion_common::Ident;
use aion_source::Span;
use serde::{Deserialize, Serialize};

// ============================================================================
// Top-level
// ============================================================================

/// A complete VHDL design file, containing one or more design units.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VhdlDesignFile {
    /// The design units in this file.
    pub units: Vec<DesignUnit>,
    /// The span covering the entire file.
    pub span: Span,
}

/// A single design unit: context items followed by a library unit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DesignUnit {
    /// A context clause followed by a design unit kind.
    ContextUnit {
        /// Library/use clauses preceding this unit.
        context: Vec<ContextItem>,
        /// The primary design unit.
        unit: DesignUnitKind,
        /// The span covering the entire unit including context.
        span: Span,
    },
    /// An error node produced during error recovery.
    Error(Span),
}

/// A context item: either a `library` or `use` clause.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ContextItem {
    /// A `library` clause (e.g., `library ieee;`).
    Library {
        /// The library names.
        names: Vec<Ident>,
        /// Source span.
        span: Span,
    },
    /// A `use` clause (e.g., `use ieee.std_logic_1164.all;`).
    Use {
        /// The selected name (e.g., `ieee.std_logic_1164.all`).
        name: SelectedName,
        /// Source span.
        span: Span,
    },
}

/// A dotted name, optionally ending in `.all`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectedName {
    /// The parts of the name (e.g., `["ieee", "std_logic_1164", "all"]`).
    pub parts: Vec<Ident>,
    /// Source span.
    pub span: Span,
}

/// The kind of a primary design unit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DesignUnitKind {
    /// An entity declaration.
    Entity(EntityDecl),
    /// An architecture body.
    Architecture(ArchitectureDecl),
    /// A package declaration.
    Package(PackageDecl),
    /// A package body.
    PackageBody(PackageBodyDecl),
}

// ============================================================================
// Entity / Architecture / Package
// ============================================================================

/// An entity declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityDecl {
    /// The entity name.
    pub name: Ident,
    /// Generic parameters, if present.
    pub generics: Option<GenericClause>,
    /// Port declarations, if present.
    pub ports: Option<PortClause>,
    /// Declarative items within the entity.
    pub decls: Vec<Declaration>,
    /// Concurrent statements in the entity body (rare, but allowed).
    pub stmts: Vec<ConcurrentStatement>,
    /// Source span.
    pub span: Span,
}

/// An architecture body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitectureDecl {
    /// The architecture name.
    pub name: Ident,
    /// The entity this architecture implements.
    pub entity_name: Ident,
    /// Declarative items in the architecture header.
    pub decls: Vec<Declaration>,
    /// Concurrent statements in the architecture body.
    pub stmts: Vec<ConcurrentStatement>,
    /// Source span.
    pub span: Span,
}

/// A package declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageDecl {
    /// The package name.
    pub name: Ident,
    /// Declarative items in the package.
    pub decls: Vec<Declaration>,
    /// Source span.
    pub span: Span,
}

/// A package body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageBodyDecl {
    /// The package name this body implements.
    pub name: Ident,
    /// Declarative items in the package body.
    pub decls: Vec<Declaration>,
    /// Source span.
    pub span: Span,
}

// ============================================================================
// Interface
// ============================================================================

/// A generic clause with interface declarations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenericClause {
    /// The generic interface declarations.
    pub decls: Vec<InterfaceDecl>,
    /// Source span.
    pub span: Span,
}

/// A port clause with interface declarations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortClause {
    /// The port interface declarations.
    pub decls: Vec<InterfaceDecl>,
    /// Source span.
    pub span: Span,
}

/// An interface declaration (generic or port).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterfaceDecl {
    /// The declared names (e.g., `a, b` in `a, b : in std_logic`).
    pub names: Vec<Ident>,
    /// The port mode (in/out/inout/buffer/linkage), or `None` for generics.
    pub mode: Option<PortMode>,
    /// The type indication.
    pub ty: TypeIndication,
    /// Optional default expression.
    pub default: Option<Expr>,
    /// Source span.
    pub span: Span,
}

/// A port direction mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PortMode {
    /// `in` — input port.
    In,
    /// `out` — output port.
    Out,
    /// `inout` — bidirectional port.
    Inout,
    /// `buffer` — output port readable internally.
    Buffer,
    /// `linkage` — linkage-mode port.
    Linkage,
}

// ============================================================================
// Declarations
// ============================================================================

/// A declarative item appearing in entity, architecture, package, or subprogram bodies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Declaration {
    /// A signal declaration.
    Signal(SignalDecl),
    /// A variable declaration.
    Variable(VariableDecl),
    /// A constant declaration.
    Constant(ConstantDecl),
    /// A type declaration.
    Type(TypeDecl),
    /// A subtype declaration.
    Subtype(SubtypeDecl),
    /// A component declaration.
    Component(ComponentDecl),
    /// A function declaration or body.
    Function(FunctionDecl),
    /// A procedure declaration or body.
    Procedure(ProcedureDecl),
    /// An alias declaration.
    Alias(AliasDecl),
    /// An attribute declaration.
    AttributeDecl(AttributeDeclNode),
    /// An attribute specification.
    AttributeSpec(AttributeSpecNode),
    /// An error node produced during error recovery.
    Error(Span),
}

/// A signal declaration (e.g., `signal clk : std_logic;`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalDecl {
    /// Signal names.
    pub names: Vec<Ident>,
    /// Signal type.
    pub ty: TypeIndication,
    /// Optional default value expression.
    pub default: Option<Expr>,
    /// Source span.
    pub span: Span,
}

/// A variable declaration (e.g., `variable count : integer := 0;`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableDecl {
    /// Whether this is a shared variable.
    pub shared: bool,
    /// Variable names.
    pub names: Vec<Ident>,
    /// Variable type.
    pub ty: TypeIndication,
    /// Optional default value expression.
    pub default: Option<Expr>,
    /// Source span.
    pub span: Span,
}

/// A constant declaration (e.g., `constant WIDTH : integer := 8;`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstantDecl {
    /// Constant names.
    pub names: Vec<Ident>,
    /// Constant type.
    pub ty: TypeIndication,
    /// Optional value expression (deferred constants omit this).
    pub value: Option<Expr>,
    /// Source span.
    pub span: Span,
}

/// A type declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeDecl {
    /// The type name.
    pub name: Ident,
    /// The type definition.
    pub def: TypeDef,
    /// Source span.
    pub span: Span,
}

/// A type definition body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TypeDef {
    /// An enumeration type (e.g., `(idle, running, stopped)`).
    Enumeration {
        /// The enumeration literals.
        literals: Vec<EnumLiteral>,
        /// Source span.
        span: Span,
    },
    /// An integer or floating-point range type (e.g., `range 0 to 255`).
    Range {
        /// The range constraint.
        constraint: RangeConstraint,
        /// Source span.
        span: Span,
    },
    /// An array type (e.g., `array (7 downto 0) of std_logic`).
    Array {
        /// Index constraints.
        indices: Vec<DiscreteRange>,
        /// Element type.
        element_type: Box<TypeIndication>,
        /// Source span.
        span: Span,
    },
    /// A record type.
    Record {
        /// Fields.
        fields: Vec<RecordField>,
        /// Source span.
        span: Span,
    },
    /// An incomplete type declaration.
    Incomplete {
        /// Source span.
        span: Span,
    },
}

/// An enumeration literal — either an identifier or a character literal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EnumLiteral {
    /// An identifier literal (e.g., `idle`).
    Ident(Ident, Span),
    /// A character literal (e.g., `'0'`).
    Char(char, Span),
}

/// A field in a record type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordField {
    /// Field names.
    pub names: Vec<Ident>,
    /// Field type.
    pub ty: TypeIndication,
    /// Source span.
    pub span: Span,
}

/// A subtype declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtypeDecl {
    /// The subtype name.
    pub name: Ident,
    /// The subtype indication.
    pub ty: TypeIndication,
    /// Source span.
    pub span: Span,
}

/// A component declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentDecl {
    /// The component name.
    pub name: Ident,
    /// Generic parameters, if present.
    pub generics: Option<GenericClause>,
    /// Port declarations, if present.
    pub ports: Option<PortClause>,
    /// Source span.
    pub span: Span,
}

/// A function declaration (or body, if `body` is `Some`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDecl {
    /// Whether this is a pure or impure function.
    pub pure: bool,
    /// The function name.
    pub name: Ident,
    /// Parameters.
    pub params: Vec<InterfaceDecl>,
    /// Return type.
    pub return_type: TypeIndication,
    /// Declarations (present only in function bodies).
    pub decls: Vec<Declaration>,
    /// Statements (present only in function bodies).
    pub stmts: Vec<SequentialStatement>,
    /// Whether this is a body (`true`) or just a declaration (`false`).
    pub has_body: bool,
    /// Source span.
    pub span: Span,
}

/// A procedure declaration (or body, if `body` is `Some`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcedureDecl {
    /// The procedure name.
    pub name: Ident,
    /// Parameters.
    pub params: Vec<InterfaceDecl>,
    /// Declarations (present only in procedure bodies).
    pub decls: Vec<Declaration>,
    /// Statements (present only in procedure bodies).
    pub stmts: Vec<SequentialStatement>,
    /// Whether this is a body (`true`) or just a declaration (`false`).
    pub has_body: bool,
    /// Source span.
    pub span: Span,
}

/// An alias declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AliasDecl {
    /// The alias name.
    pub name: Ident,
    /// Optional type indication.
    pub ty: Option<TypeIndication>,
    /// The aliased expression/name.
    pub value: Expr,
    /// Source span.
    pub span: Span,
}

/// An attribute declaration (e.g., `attribute syn_keep : boolean;`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributeDeclNode {
    /// The attribute name.
    pub name: Ident,
    /// The attribute type.
    pub ty: TypeIndication,
    /// Source span.
    pub span: Span,
}

/// An attribute specification (e.g., `attribute syn_keep of clk : signal is true;`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributeSpecNode {
    /// The attribute name.
    pub name: Ident,
    /// The entity designator.
    pub entity: Ident,
    /// The entity class (signal, variable, etc.).
    pub entity_class: Ident,
    /// The attribute value expression.
    pub value: Expr,
    /// Source span.
    pub span: Span,
}

// ============================================================================
// Types
// ============================================================================

/// A type indication (type mark with optional constraint).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeIndication {
    /// The type mark (e.g., `std_logic_vector`).
    pub type_mark: SelectedName,
    /// Optional constraint (e.g., `(7 downto 0)`).
    pub constraint: Option<Constraint>,
    /// Source span.
    pub span: Span,
}

/// A constraint on a type indication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Constraint {
    /// A range constraint (e.g., `range 0 to 255`).
    Range(RangeConstraint),
    /// An index constraint (e.g., `(7 downto 0)` or `(0 to 3, 0 to 7)`).
    Index(Vec<DiscreteRange>, Span),
}

/// A range constraint with direction (e.g., `7 downto 0`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RangeConstraint {
    /// Left bound expression.
    pub left: Box<Expr>,
    /// Direction.
    pub direction: RangeDirection,
    /// Right bound expression.
    pub right: Box<Expr>,
    /// Source span.
    pub span: Span,
}

/// Direction in a range constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RangeDirection {
    /// Ascending range (`to`).
    To,
    /// Descending range (`downto`).
    Downto,
}

/// A discrete range — either an explicit range or a type-indication-based range.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DiscreteRange {
    /// An explicit range (e.g., `7 downto 0`).
    Range(RangeConstraint),
    /// A type indication used as a range (e.g., `integer range 0 to 255`).
    TypeIndication(TypeIndication),
}

// ============================================================================
// Concurrent Statements
// ============================================================================

/// A concurrent statement in an architecture or entity body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConcurrentStatement {
    /// A process statement.
    Process(ProcessStatement),
    /// A concurrent signal assignment.
    SignalAssignment(ConcurrentSignalAssignment),
    /// A component instantiation.
    ComponentInstantiation(ComponentInstantiation),
    /// A for-generate statement.
    ForGenerate(ForGenerate),
    /// An if-generate statement.
    IfGenerate(IfGenerate),
    /// A concurrent assertion.
    Assert(ConcurrentAssert),
    /// An error node produced during error recovery.
    Error(Span),
}

/// A process statement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessStatement {
    /// Optional label.
    pub label: Option<Ident>,
    /// Sensitivity list.
    pub sensitivity: SensitivityList,
    /// Process declarations.
    pub decls: Vec<Declaration>,
    /// Sequential statements.
    pub stmts: Vec<SequentialStatement>,
    /// Source span.
    pub span: Span,
}

/// A sensitivity list for a process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SensitivityList {
    /// `process(all)` — VHDL-2008 implicit sensitivity.
    All,
    /// An explicit list of signals.
    List(Vec<SelectedName>),
    /// No sensitivity list (combinational process with wait).
    None,
}

/// A concurrent signal assignment (e.g., `y <= a and b;`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConcurrentSignalAssignment {
    /// Optional label.
    pub label: Option<Ident>,
    /// Target signal.
    pub target: Expr,
    /// Value expression(s) with optional conditions.
    pub waveforms: Vec<Waveform>,
    /// Source span.
    pub span: Span,
}

/// A waveform element in a signal assignment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Waveform {
    /// The value expression.
    pub value: Expr,
    /// Optional `after` time expression.
    pub after: Option<Expr>,
    /// Source span.
    pub span: Span,
}

/// A component instantiation (e.g., `u1 : counter port map (...);`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentInstantiation {
    /// The instance label.
    pub label: Ident,
    /// The instantiated unit name.
    pub unit: InstantiatedUnit,
    /// Generic map, if present.
    pub generic_map: Option<AssociationList>,
    /// Port map, if present.
    pub port_map: Option<AssociationList>,
    /// Source span.
    pub span: Span,
}

/// The unit being instantiated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InstantiatedUnit {
    /// Component instantiation (just a name).
    Component(SelectedName),
    /// Entity instantiation (`entity lib.entity(arch)`).
    Entity(SelectedName, Option<Ident>),
}

/// An association list (generic map or port map).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssociationList {
    /// The association elements.
    pub elements: Vec<AssociationElement>,
    /// Source span.
    pub span: Span,
}

/// An element in an association list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssociationElement {
    /// The formal part (e.g., port name), if named association.
    pub formal: Option<Expr>,
    /// The actual part (the connected signal/expression).
    pub actual: Expr,
    /// Source span.
    pub span: Span,
}

/// A for-generate statement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForGenerate {
    /// The generate label.
    pub label: Ident,
    /// The loop variable name.
    pub var: Ident,
    /// The range to iterate over.
    pub range: DiscreteRange,
    /// Concurrent statements in the generate body.
    pub stmts: Vec<ConcurrentStatement>,
    /// Source span.
    pub span: Span,
}

/// An if-generate statement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IfGenerate {
    /// The generate label.
    pub label: Ident,
    /// The condition expression.
    pub condition: Expr,
    /// Concurrent statements in the `then` branch.
    pub then_stmts: Vec<ConcurrentStatement>,
    /// Optional `else generate` statements.
    pub else_stmts: Vec<ConcurrentStatement>,
    /// Source span.
    pub span: Span,
}

/// A concurrent assertion statement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConcurrentAssert {
    /// Optional label.
    pub label: Option<Ident>,
    /// The assertion condition.
    pub condition: Expr,
    /// Optional report string.
    pub report: Option<Expr>,
    /// Optional severity level.
    pub severity: Option<Expr>,
    /// Source span.
    pub span: Span,
}

// ============================================================================
// Sequential Statements
// ============================================================================

/// A sequential statement in a process or subprogram body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SequentialStatement {
    /// A signal assignment (e.g., `q <= d;`).
    SignalAssignment {
        /// Target signal.
        target: Expr,
        /// Waveform values.
        waveforms: Vec<Waveform>,
        /// Source span.
        span: Span,
    },
    /// A variable assignment (e.g., `count := count + 1;`).
    VariableAssignment {
        /// Target variable.
        target: Expr,
        /// Value expression.
        value: Expr,
        /// Source span.
        span: Span,
    },
    /// An if statement.
    If(IfStatement),
    /// A case statement.
    Case(CaseStatement),
    /// A for loop.
    ForLoop(ForLoop),
    /// A while loop.
    WhileLoop(WhileLoop),
    /// A plain loop.
    Loop(LoopStatement),
    /// A `next` statement.
    Next {
        /// Optional label to next to.
        label: Option<Ident>,
        /// Optional condition.
        condition: Option<Expr>,
        /// Source span.
        span: Span,
    },
    /// An `exit` statement.
    Exit {
        /// Optional label to exit from.
        label: Option<Ident>,
        /// Optional condition.
        condition: Option<Expr>,
        /// Source span.
        span: Span,
    },
    /// A `return` statement.
    Return {
        /// Optional return value.
        value: Option<Expr>,
        /// Source span.
        span: Span,
    },
    /// A `wait` statement.
    Wait(WaitStatement),
    /// An assertion statement.
    Assert {
        /// The assertion condition.
        condition: Expr,
        /// Optional report string.
        report: Option<Expr>,
        /// Optional severity level.
        severity: Option<Expr>,
        /// Source span.
        span: Span,
    },
    /// A `report` statement.
    Report {
        /// The report message.
        message: Expr,
        /// Optional severity level.
        severity: Option<Expr>,
        /// Source span.
        span: Span,
    },
    /// A `null` statement.
    Null {
        /// Source span.
        span: Span,
    },
    /// A procedure call.
    ProcedureCall {
        /// The procedure name.
        name: Expr,
        /// Arguments.
        args: Option<AssociationList>,
        /// Source span.
        span: Span,
    },
    /// An error node produced during error recovery.
    Error(Span),
}

/// An if statement with optional elsif/else branches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IfStatement {
    /// Optional label.
    pub label: Option<Ident>,
    /// The condition.
    pub condition: Expr,
    /// The `then` branch statements.
    pub then_stmts: Vec<SequentialStatement>,
    /// Optional `elsif` branches.
    pub elsif_branches: Vec<ElsifBranch>,
    /// Optional `else` branch statements.
    pub else_stmts: Vec<SequentialStatement>,
    /// Source span.
    pub span: Span,
}

/// An `elsif` branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElsifBranch {
    /// The condition.
    pub condition: Expr,
    /// The branch statements.
    pub stmts: Vec<SequentialStatement>,
    /// Source span.
    pub span: Span,
}

/// A case statement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseStatement {
    /// Optional label.
    pub label: Option<Ident>,
    /// The expression being matched.
    pub expr: Expr,
    /// The case alternatives.
    pub alternatives: Vec<CaseAlternative>,
    /// Source span.
    pub span: Span,
}

/// A single alternative in a case statement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseAlternative {
    /// The choice(s) for this alternative.
    pub choices: Vec<Choice>,
    /// The statements to execute.
    pub stmts: Vec<SequentialStatement>,
    /// Source span.
    pub span: Span,
}

/// A choice in a case alternative.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Choice {
    /// An expression choice.
    Expr(Expr),
    /// A range choice.
    Range(RangeConstraint),
    /// The `others` keyword.
    Others(Span),
}

/// A for loop statement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForLoop {
    /// Optional label.
    pub label: Option<Ident>,
    /// The loop variable name.
    pub var: Ident,
    /// The iteration range.
    pub range: DiscreteRange,
    /// Loop body statements.
    pub stmts: Vec<SequentialStatement>,
    /// Source span.
    pub span: Span,
}

/// A while loop statement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhileLoop {
    /// Optional label.
    pub label: Option<Ident>,
    /// The condition expression.
    pub condition: Expr,
    /// Loop body statements.
    pub stmts: Vec<SequentialStatement>,
    /// Source span.
    pub span: Span,
}

/// A plain loop statement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopStatement {
    /// Optional label.
    pub label: Option<Ident>,
    /// Loop body statements.
    pub stmts: Vec<SequentialStatement>,
    /// Source span.
    pub span: Span,
}

/// A wait statement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitStatement {
    /// Optional sensitivity clause (`wait on ...`).
    pub on: Vec<SelectedName>,
    /// Optional condition clause (`wait until ...`).
    pub until: Option<Expr>,
    /// Optional timeout clause (`wait for ...`).
    pub duration: Option<Expr>,
    /// Source span.
    pub span: Span,
}

// ============================================================================
// Expressions
// ============================================================================

/// An expression node in the VHDL AST.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Expr {
    /// A name reference (possibly qualified with dots, indices, attributes).
    Name(Name),
    /// An integer literal.
    IntLiteral {
        /// Source span.
        span: Span,
    },
    /// A real literal.
    RealLiteral {
        /// Source span.
        span: Span,
    },
    /// A character literal.
    CharLiteral {
        /// Source span.
        span: Span,
    },
    /// A string literal.
    StringLiteral {
        /// Source span.
        span: Span,
    },
    /// A bit string literal.
    BitStringLiteral {
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
    /// A unary operation.
    Unary {
        /// The operator.
        op: UnaryOp,
        /// The operand.
        operand: Box<Expr>,
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
    /// An aggregate (e.g., `(others => '0')`).
    Aggregate {
        /// The elements.
        elements: Vec<AggregateElement>,
        /// Source span.
        span: Span,
    },
    /// A qualified expression (e.g., `std_logic'('1')`).
    Qualified {
        /// The type mark.
        type_mark: SelectedName,
        /// The inner expression.
        expr: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// A type conversion (e.g., `integer(x)`).
    TypeConversion {
        /// The target type.
        type_mark: SelectedName,
        /// The expression to convert.
        expr: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// A function call (e.g., `to_unsigned(val, 8)`).
    FunctionCall {
        /// The function name.
        name: Box<Expr>,
        /// The arguments.
        args: AssociationList,
        /// Source span.
        span: Span,
    },
    /// An attribute reference (e.g., `clk'event`).
    Attribute {
        /// The prefix expression.
        prefix: Box<Expr>,
        /// The attribute name.
        attr: Ident,
        /// Optional argument.
        arg: Option<Box<Expr>>,
        /// Source span.
        span: Span,
    },
    /// The `others` keyword (used in aggregates).
    Others {
        /// Source span.
        span: Span,
    },
    /// The `open` keyword (used in port maps).
    Open {
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
            Expr::Name(n) => n.span,
            Expr::IntLiteral { span }
            | Expr::RealLiteral { span }
            | Expr::CharLiteral { span }
            | Expr::StringLiteral { span }
            | Expr::BitStringLiteral { span }
            | Expr::Binary { span, .. }
            | Expr::Unary { span, .. }
            | Expr::Paren { span, .. }
            | Expr::Aggregate { span, .. }
            | Expr::Qualified { span, .. }
            | Expr::TypeConversion { span, .. }
            | Expr::FunctionCall { span, .. }
            | Expr::Attribute { span, .. }
            | Expr::Others { span }
            | Expr::Open { span }
            | Expr::Error(span) => *span,
        }
    }
}

/// An element in an aggregate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregateElement {
    /// Optional choices (e.g., `0 =>`, `others =>`).
    pub choices: Vec<Choice>,
    /// The element value expression.
    pub value: Expr,
    /// Source span.
    pub span: Span,
}

/// A name with optional suffixes (dot selection, indexing, slicing, attributes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Name {
    /// The primary identifier.
    pub primary: Ident,
    /// Suffixes applied to the name.
    pub parts: Vec<NameSuffix>,
    /// Source span.
    pub span: Span,
}

/// A suffix on a name reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NameSuffix {
    /// A dot selection (e.g., `.field`).
    Selected(Ident, Span),
    /// Index or function call (e.g., `(0)`, `(a, b)`).
    Index(Vec<Expr>, Span),
    /// A slice (e.g., `(7 downto 0)`).
    Slice(RangeConstraint, Span),
    /// An attribute (e.g., `'event`, `'range`).
    Attribute(Ident, Option<Box<Expr>>, Span),
    /// `.all` selection.
    All(Span),
}

/// A binary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BinaryOp {
    /// `and`
    And,
    /// `or`
    Or,
    /// `nand`
    Nand,
    /// `nor`
    Nor,
    /// `xor`
    Xor,
    /// `xnor`
    Xnor,
    /// `=`
    Eq,
    /// `/=`
    Neq,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `?=`
    MatchEq,
    /// `?/=`
    MatchNeq,
    /// `?<`
    MatchLt,
    /// `?<=`
    MatchLe,
    /// `?>`
    MatchGt,
    /// `?>=`
    MatchGe,
    /// `sll`
    Sll,
    /// `srl`
    Srl,
    /// `sla`
    Sla,
    /// `sra`
    Sra,
    /// `rol`
    Rol,
    /// `ror`
    Ror,
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `&`
    Concat,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `mod`
    Mod,
    /// `rem`
    Rem2,
    /// `**`
    Pow,
}

/// A unary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnaryOp {
    /// `not`
    Not,
    /// `abs`
    Abs,
    /// `+` (unary plus)
    Pos,
    /// `-` (unary minus)
    Neg,
    /// `??` (condition operator)
    Condition,
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_source::FileId;

    fn dummy_span() -> Span {
        Span::new(FileId::from_raw(0), 0, 1)
    }

    fn dummy_name() -> SelectedName {
        SelectedName {
            parts: vec![Ident::from_raw(0)],
            span: dummy_span(),
        }
    }

    #[test]
    fn serde_roundtrip_expr() {
        let expr = Expr::IntLiteral { span: dummy_span() };
        let json = serde_json::to_string(&expr).unwrap();
        let back: Expr = serde_json::from_str(&json).unwrap();
        assert_eq!(back.span(), dummy_span());
    }

    #[test]
    fn serde_roundtrip_entity() {
        let entity = EntityDecl {
            name: Ident::from_raw(0),
            generics: None,
            ports: None,
            decls: Vec::new(),
            stmts: Vec::new(),
            span: dummy_span(),
        };
        let json = serde_json::to_string(&entity).unwrap();
        let back: EntityDecl = serde_json::from_str(&json).unwrap();
        assert_eq!(back.span, dummy_span());
    }

    #[test]
    fn serde_roundtrip_design_file() {
        let file = VhdlDesignFile {
            units: Vec::new(),
            span: dummy_span(),
        };
        let json = serde_json::to_string(&file).unwrap();
        let back: VhdlDesignFile = serde_json::from_str(&json).unwrap();
        assert!(back.units.is_empty());
    }

    #[test]
    fn serde_roundtrip_type_indication() {
        let ti = TypeIndication {
            type_mark: dummy_name(),
            constraint: None,
            span: dummy_span(),
        };
        let json = serde_json::to_string(&ti).unwrap();
        let back: TypeIndication = serde_json::from_str(&json).unwrap();
        assert_eq!(back.span, dummy_span());
    }

    #[test]
    fn expr_span_accessor() {
        let span = dummy_span();
        assert_eq!(Expr::IntLiteral { span }.span(), span);
        assert_eq!(Expr::Error(span).span(), span);
        assert_eq!(Expr::Others { span }.span(), span);
        let name = Expr::Name(Name {
            primary: Ident::from_raw(0),
            parts: Vec::new(),
            span,
        });
        assert_eq!(name.span(), span);
    }
}
