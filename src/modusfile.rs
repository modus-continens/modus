// Copyright 2021 Sergey Mechtaev

// This file is part of Modus.

// Modus is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Modus is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Modus.  If not, see <https://www.gnu.org/licenses/>.

use nom::character::complete::line_ending;
use nom::character::complete::not_line_ending;
use nom::error::convert_error;
use nom::error::VerboseError;
use std::fmt;
use std::str;

use crate::dockerfile;
use crate::logic;
use crate::logic::parser::Span;
use crate::logic::SpannedPosition;

#[derive(Clone, PartialEq, Debug)]
pub enum Expression {
    Literal(Literal),

    // An operator applied to an expression
    OperatorApplication(Option<SpannedPosition>, Box<Expression>, Operator),

    // A conjunction of expressions.
    And(Option<SpannedPosition>, Box<Expression>, Box<Expression>),

    // A disjunction of expressions.
    Or(Option<SpannedPosition>, Box<Expression>, Box<Expression>),
}

impl Expression {
    #[cfg(test)]
    fn eq_ignoring_position(&self, other: &Expression) -> bool {
        match (self, other) {
            (Expression::Literal(l), Expression::Literal(r)) => l.eq_ignoring_position(&r),
            (
                Expression::OperatorApplication(_, l, _),
                Expression::OperatorApplication(_, r, _),
            ) => l.eq_ignoring_position(r),
            (Expression::And(_, l1, l2), Expression::And(_, r1, r2))
            | (Expression::Or(_, l1, l2), Expression::Or(_, r1, r2)) => {
                l1.eq_ignoring_position(r1) && l2.eq_ignoring_position(r2)
            }
            (s, o) => s.eq(o),
        }
    }

    pub fn get_spanned_position(&self) -> &Option<SpannedPosition> {
        match self {
            Expression::Literal(lit) => &lit.position,
            Expression::OperatorApplication(s, _, _) => &s,
            Expression::And(s, _, _) => &s,
            Expression::Or(s, _, _) => &s,
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct ModusClause {
    pub head: Literal,
    // If None, this clause is a fact.
    pub body: Option<Expression>,
}

#[cfg(test)]
impl ModusClause {
    fn eq_ignoring_position(&self, other: &ModusClause) -> bool {
        if let (Some(expr1), Some(expr2)) = (&self.body, &other.body) {
            self.head.eq_ignoring_position(&other.head) && expr1.eq_ignoring_position(&expr2)
        } else {
            self.head.eq_ignoring_position(&other.head) && self.body.eq(&other.body)
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ModusTerm {
    Constant(String),
    /// A format string with '\$' left unhandled. This should be dealt with when
    /// converting to the IR.
    FormatString {
        /// The position of this term, beginning from the 'f' in the source
        position: SpannedPosition,
        /// The input string literal, as in the source code, i.e. the escape characters
        /// have not been converted.
        format_string_literal: String,
    },
    UserVariable(String),
}

impl fmt::Display for ModusTerm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModusTerm::Constant(s) => write!(f, "\"{}\"", s),
            ModusTerm::UserVariable(s) => write!(f, "{}", s),
            ModusTerm::FormatString {
                position: _,
                format_string_literal,
            } => write!(f, "\"{}\"", format_string_literal),
        }
    }
}

impl From<ModusTerm> for logic::IRTerm {
    fn from(modus_term: ModusTerm) -> Self {
        match modus_term {
            ModusTerm::Constant(c) => logic::IRTerm::Constant(c),
            // TODO: return a Result type and propogate the error up properly if we want to warn about
            // this. Example: the user might try to use a format string in a head literal.
            ModusTerm::FormatString {
                position: _,
                format_string_literal: _,
            } => panic!("Cannot convert a format string to an IRTerm."),
            ModusTerm::UserVariable(v) => logic::IRTerm::UserVariable(v),
        }
    }
}

impl fmt::Display for logic::Literal<ModusTerm> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &*self.args {
            [] => write!(f, "{}", self.predicate),
            _ => write!(
                f,
                "{}({})",
                self.predicate,
                self.args
                    .iter()
                    .map(|a| a.to_string())
                    .collect::<Vec<String>>()
                    .join(", ")
            ),
        }
    }
}

type Literal = logic::Literal<ModusTerm>;

impl From<Literal> for logic::Literal {
    fn from(modus_literal: Literal) -> Self {
        Self {
            position: modus_literal.position,
            predicate: modus_literal.predicate,
            args: modus_literal
                .args
                .into_iter()
                .map(|arg| arg.into())
                .collect(),
        }
    }
}

type Fact = ModusClause;
type Rule = ModusClause;
pub type Operator = Literal;

#[derive(Clone, PartialEq, Debug)]
pub struct Modusfile(pub Vec<ModusClause>);

#[derive(Clone, PartialEq, Debug)]
pub struct Version {
    major: u32,
    minor: u32,
    patch: u32,
    pre_release: String,
    build: String,
}

impl str::FromStr for Modusfile {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let span = Span::new(s);
        match parser::modusfile(span) {
            Result::Ok((_, o)) => Ok(o),
            Result::Err(nom::Err::Error(e) | nom::Err::Failure(e)) => {
                // HACK: converts our span based errors to the standard &str based errors.
                // This drops information that would likely be recomputed in `convert_error`.
                let errors = e
                    .errors
                    .into_iter()
                    .map(|(span, err)| (span.fragment().to_owned(), err))
                    .collect();
                let str_verbose_error = VerboseError { errors };
                Result::Err(convert_error(s, str_verbose_error))
            }
            _ => unimplemented!(),
        }
    }
}

impl fmt::Display for Expression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expression::OperatorApplication(_, expr, op) => {
                write!(f, "({})::{}", expr.to_string(), op)
            }
            Expression::Literal(l) => write!(f, "{}", l.to_string()),
            Expression::And(_, expr1, expr2) => {
                // Explicit parenthesization when printing to output, looks a bit
                // verbose but shouldn't affect user code.
                write!(f, "({}, {})", expr1, expr2)
            }
            Expression::Or(_, expr1, expr2) => {
                write!(f, "({}; {})", expr1, expr2)
            }
        }
    }
}

// could write a macro that generates these
impl From<Literal> for Expression {
    fn from(l: Literal) -> Self {
        Expression::Literal(l)
    }
}

impl str::FromStr for ModusClause {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let span = Span::new(s);
        match parser::modus_clause(span) {
            Result::Ok((_, o)) => Ok(o),
            Result::Err(e) => Result::Err(format!("{}", e)),
        }
    }
}

impl fmt::Display for ModusClause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(e) = &self.body {
            write!(f, "{} :- {}.", self.head, e.to_string(),)
        } else {
            write!(f, "{}.", self.head)
        }
    }
}

impl str::FromStr for Literal {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let span = Span::new(s);
        match logic::parser::literal(parser::modus_term)(span) {
            Result::Ok((_, o)) => Ok(o),
            Result::Err(e) => Result::Err(format!("{}", e)),
        }
    }
}

pub mod parser {
    use crate::logic::parser::{literal, literal_identifier, recognized_span, IResult};
    use crate::logic::Predicate;

    use super::*;

    use nom::bytes::complete::escaped;
    use nom::character::complete::{multispace0, none_of, one_of};
    use nom::combinator::{cut, opt, recognize};
    use nom::error::context;
    use nom::multi::separated_list1;
    use nom::{
        branch::alt,
        bytes::complete::tag,
        character::complete::space0,
        combinator::{eof, map},
        multi::{many0, separated_list0},
        sequence::{delimited, preceded, separated_pair, terminated},
    };

    fn comment(s: Span) -> IResult<Span, Span> {
        delimited(tag("#"), not_line_ending, line_ending)(s)
    }

    fn comments(s: Span) -> IResult<Span, Vec<Span>> {
        delimited(
            multispace0,
            separated_list0(multispace0, comment),
            multispace0,
        )(s)
    }

    fn head(i: Span) -> IResult<Span, Literal> {
        context(stringify!(head), literal(modus_term))(i)
    }

    /// Parses `<term1> = <term2>` into a builtin call, `string_eq(term1, term2)`.
    fn unification_sugar(i: Span) -> IResult<Span, Literal> {
        let (i, (spanned_pos, (t1, t2))) = recognized_span(separated_pair(
            modus_term,
            delimited(multispace0, tag("="), multispace0),
            cut(modus_term),
        ))(i)?;

        Ok((
            i,
            Literal {
                position: Some(spanned_pos),
                predicate: Predicate("string_eq".to_owned()),
                args: vec![t1, t2],
            },
        ))
    }

    fn modus_literal(i: Span) -> IResult<Span, Expression> {
        map(literal(modus_term), Expression::Literal)(i)
    }

    fn expression_inner(i: Span) -> IResult<Span, Expression> {
        let l_paren_with_comments = |i| terminated(tag("("), comments)(i);
        let r_paren_with_comments = |i| preceded(comments, cut(tag(")")))(i);

        let unification_expr_parser = map(unification_sugar, Expression::Literal);
        // These inner expression parsers can fully recurse.
        let op_application_parser = map(
            recognized_span(separated_pair(
                alt((
                    modus_literal,
                    delimited(l_paren_with_comments, body, r_paren_with_comments),
                )),
                tag("::"),
                cut(literal(modus_term)),
            )),
            |(span, (expr, operator))| {
                Expression::OperatorApplication(Some(span), Box::new(expr), operator)
            },
        );
        let parenthesized_expr = delimited(l_paren_with_comments, body, r_paren_with_comments);
        alt((
            unification_expr_parser,
            op_application_parser,
            modus_literal,
            parenthesized_expr,
        ))(i)
    }

    fn body(i: Span) -> IResult<Span, Expression> {
        let comma_separated_exprs = map(
            separated_list1(delimited(comments, tag(","), comments), expression_inner),
            |es| {
                es.into_iter()
                    .reduce(|e1, e2| {
                        // this was just parsed from source code, so it should have a position
                        let s1 = e1.get_spanned_position().as_ref().unwrap();
                        let s2 = e2.get_spanned_position().as_ref().unwrap();

                        // This span should include the comma/spacing/etc between e1/e2
                        let computed_span = SpannedPosition {
                            offset: s1.offset,
                            length: s2.offset + s2.length - s1.offset,
                        };
                        Expression::And(Some(computed_span), Box::new(e1), Box::new(e2))
                    })
                    .expect("Converting list to expression pairs.")
            },
        );
        let semi_separated_exprs = map(
            separated_list1(
                delimited(comments, tag(";"), comments),
                comma_separated_exprs,
            ),
            |es| {
                es.into_iter()
                    .reduce(|e1, e2| {
                        let s1 = e1.get_spanned_position().as_ref().unwrap();
                        let s2 = e2.get_spanned_position().as_ref().unwrap();

                        let computed_span = SpannedPosition {
                            offset: s1.offset,
                            length: s2.offset + s2.length - s1.offset,
                        };
                        Expression::Or(Some(computed_span), Box::new(e1), Box::new(e2))
                    })
                    .expect("Converting list to expression pairs.")
            },
        );
        // Parses the body as a semicolon separated list of comma separated inner expressions.
        // This resolves ambiguity by making commas/and higher precedence.
        preceded(comments, semi_separated_exprs)(i)
    }

    fn fact(i: Span) -> IResult<Span, ModusClause> {
        // Custom definition of fact since datalog facts are normally "head :- ", but Moduslog
        // defines it as "head."
        context(
            stringify!(fact),
            map(terminated(head, tag(".")), |h| ModusClause {
                head: h,
                body: None,
            }),
        )(i)
    }

    fn rule(i: Span) -> IResult<Span, ModusClause> {
        context(
            stringify!(rule),
            map(
                separated_pair(
                    head,
                    delimited(space0, tag(":-"), multispace0),
                    context("rule_body", cut(terminated(body, tag(".")))),
                ),
                |(head, body)| ModusClause {
                    head,
                    body: Some(body),
                },
            ),
        )(i)
    }

    /// Processes the given string, converting escape substrings into the proper characters.
    ///
    /// This also supports string continuation, This allows users to write strings like: "Hello, \
    ///                                                                                   World!"
    /// which is actually just "Hello, World!".
    pub fn process_raw_string(s: &str) -> String {
        let mut processed = String::new();

        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('"') => processed.push('"'),
                    Some('\\') => processed.push('\\'),
                    Some('n') => processed.push('\n'),
                    Some('r') => processed.push('\r'),
                    Some('t') => processed.push('\t'),
                    Some('0') => processed.push('\0'),
                    Some('\n') => {
                        // string continuation so we'll ignore whitespace till we get to a non-whitespace.
                        while let Some(c) = chars.peek() {
                            if !c.is_whitespace() {
                                break;
                            }
                            chars.next();
                        }
                    }
                    Some(c) => {
                        // leave it unchanged if we don't recognize the escape char
                        processed.push('\\');
                        processed.push(c);
                    }
                    None => panic!("given string ends with an escape character"),
                }
            } else {
                processed.push(c);
            }
        }
        processed
    }

    /// Parses a string that possibly contains escaped characters, but doesn't actually
    /// convert the escape characters.
    fn string_content(i: Span) -> IResult<Span, String> {
        let escape_parser = escaped(none_of("\\\""), '\\', one_of("\"\\nrt0\n"));
        let (i, o) = opt(escape_parser)(i)?;
        let parsed_str: &str = o.map(|span| *span.fragment()).unwrap_or("");
        Ok((i, parsed_str.to_owned()))
    }

    pub fn modus_const(i: Span) -> IResult<Span, String> {
        context(
            stringify!(modus_const),
            delimited(tag("\""), string_content, cut(tag("\""))),
        )(i)
    }

    /// Parses a substring outside of the expansion part of a format string's content.
    pub fn outside_format_expansion(i: Span) -> IResult<Span, Span> {
        // We want to parse until we see '$', except if it was preceded with escape char.
        recognize(opt(escaped(none_of("\\$"), '\\', one_of("$"))))(i)
    }

    fn modus_format_string(i: Span) -> IResult<Span, (SpannedPosition, String)> {
        context(
            stringify!(modus_format_string),
            // TODO: check that the token(s) inside "${...}" conform to the variable syntax.
            // Note that if it's escaped, "\${...}", it does not need to conform to the variable syntax.
            recognized_span(delimited(tag("f\""), string_content, cut(tag("\"")))),
        )(i)
    }

    pub fn variable_identifier(i: Span) -> IResult<Span, Span> {
        literal_identifier(i)
    }

    pub fn modus_var(i: Span) -> IResult<Span, Span> {
        context(stringify!(modus_var), variable_identifier)(i)
    }

    pub fn modus_term(i: Span) -> IResult<Span, ModusTerm> {
        alt((
            map(modus_const, ModusTerm::Constant),
            map(modus_format_string, |(position, format_string_literal)| {
                ModusTerm::FormatString {
                    position,
                    format_string_literal,
                }
            }),
            map(modus_var, |s| {
                ModusTerm::UserVariable(s.fragment().to_string())
            }),
        ))(i)
    }

    pub fn modus_clause(i: Span) -> IResult<Span, ModusClause> {
        alt((fact, rule))(i)
    }

    pub fn modusfile(i: Span) -> IResult<Span, Modusfile> {
        map(
            terminated(
                many0(preceded(
                    many0(dockerfile::parser::ignored_line_for_span),
                    modus_clause,
                )),
                terminated(many0(dockerfile::parser::ignored_line_for_span), eof),
            ),
            Modusfile,
        )(i)
    }
}

#[cfg(test)]
mod tests {
    use serial_test::serial;

    use super::*;

    #[test]
    fn fact() {
        let l1 = Literal {
            position: None,
            predicate: logic::Predicate("l1".into()),
            args: Vec::new(),
        };
        let c = ModusClause {
            head: l1,
            body: None,
        };

        assert_eq!("l1.", c.to_string());

        let actual: ModusClause = "l1.".parse().unwrap();
        assert!(c.eq_ignoring_position(&actual));
    }

    #[test]
    fn rule() {
        let l1 = Literal {
            position: None,
            predicate: logic::Predicate("l1".into()),
            args: Vec::new(),
        };
        let l2 = Literal {
            position: None,
            predicate: logic::Predicate("l2".into()),
            args: Vec::new(),
        };
        let l3 = Literal {
            position: None,
            predicate: logic::Predicate("l3".into()),
            args: Vec::new(),
        };
        let c = Rule {
            head: l1,
            body: Expression::And(None, Box::new(l2.into()), Box::new(l3.into())).into(),
        };

        assert_eq!("l1 :- (l2, l3).", c.to_string());

        let actual1: Rule = "l1 :- l2, l3.".parse().unwrap();
        assert!(c.eq_ignoring_position(&actual1));
        let actual2: Rule = "l1 :- l2,\n\tl3.".parse().unwrap();
        assert!(c.eq_ignoring_position(&actual2));
    }

    #[test]
    fn rule_with_or() {
        let l1: Literal = "l1".parse().unwrap();
        let l2: Literal = "l2".parse().unwrap();
        let c = Rule {
            head: "foo".parse().unwrap(),
            body: Expression::Or(None, Box::new(l1.into()), Box::new(l2.into())).into(),
        };

        assert_eq!("foo :- (l1; l2).", c.to_string());

        let actual: Rule = "foo :- l1; l2.".parse().unwrap();
        assert!(c.eq_ignoring_position(&actual));
    }

    #[test]
    fn rule_with_operator() {
        let foo = Literal {
            position: None,
            predicate: logic::Predicate("foo".into()),
            args: Vec::new(),
        };
        let a = Literal {
            position: None,
            predicate: logic::Predicate("a".into()),
            args: Vec::new(),
        };
        let b = Literal {
            position: None,
            predicate: logic::Predicate("b".into()),
            args: Vec::new(),
        };
        let merge = Operator {
            position: None,
            predicate: logic::Predicate("merge".into()),
            args: Vec::new(),
        };
        let r1 = Rule {
            head: foo.clone(),
            body: Expression::OperatorApplication(
                None,
                Expression::And(None, Box::new(a.clone().into()), Box::new(b.into())).into(),
                merge.clone(),
            )
            .into(),
        };
        let r2 = Rule {
            head: foo,
            body: Expression::OperatorApplication(None, Box::new(Expression::Literal(a)), merge)
                .into(),
        };

        assert_eq!("foo :- ((a, b))::merge.", r1.to_string());
        let actual1 = "foo :- ((a, b))::merge.".parse().unwrap();
        assert!(r1.eq_ignoring_position(&actual1));
        let actual2 = "foo :- (a, b)::merge.".parse().unwrap();
        assert!(r1.eq_ignoring_position(&actual2));

        assert_eq!("foo :- (a)::merge.", r2.to_string());
        let actual3 = "foo :- a::merge.".parse().unwrap();
        assert!(r2.eq_ignoring_position(&actual3));
        let actual3 = "foo :- ( a )::merge.".parse().unwrap();
        assert!(r2.eq_ignoring_position(&actual3));
    }

    #[test]
    #[serial]
    fn modusclause_to_clause() {
        crate::translate::reset_operator_pair_id();
        let foo = Literal {
            position: None,
            predicate: logic::Predicate("foo".into()),
            args: Vec::new(),
        };
        let a = Literal {
            position: None,
            predicate: logic::Predicate("a".into()),
            args: Vec::new(),
        };
        let b = Literal {
            position: None,
            predicate: logic::Predicate("b".into()),
            args: Vec::new(),
        };
        let merge = Operator {
            position: None,
            predicate: logic::Predicate("merge".into()),
            args: Vec::new(),
        };
        let r = Rule {
            head: foo,
            body: Expression::OperatorApplication(
                None,
                Expression::And(None, Box::new(a.into()), Box::new(b.into())).into(),
                merge,
            )
            .into(),
        };
        assert_eq!("foo :- ((a, b))::merge.", r.to_string());

        // Convert to the simpler syntax
        let c: Vec<logic::Clause> = (&r).into();
        assert_eq!(1, c.len());
        assert_eq!(
            r#"foo :- _operator_merge_begin("0"), a, b, _operator_merge_end("0")"#,
            c[0].to_string()
        );
    }

    #[test]
    #[serial]
    fn modusclause_to_clause_with_or() {
        crate::translate::reset_operator_pair_id();
        let foo: Literal = "foo".parse().unwrap();
        let a: Literal = "a".parse().unwrap();
        let b: Literal = "b".parse().unwrap();
        let merge = Operator {
            position: None,
            predicate: logic::Predicate("merge".into()),
            args: Vec::new(),
        };
        let r1 = Rule {
            head: foo.clone(),
            body: Expression::OperatorApplication(
                None,
                Expression::Or(None, Box::new(a.clone().into()), Box::new(b.clone().into())).into(),
                merge,
            )
            .into(),
        };
        let r2 = Rule {
            head: foo.clone(),
            body: Expression::And(
                None,
                Box::new(a.clone().into()),
                Box::new(Expression::And(
                    None,
                    Box::new(b.clone().into()),
                    Box::new(Expression::Or(
                        None,
                        Box::new(a.clone().into()),
                        Box::new(b.clone().into()),
                    )),
                )),
            )
            .into(),
        };
        assert_eq!("foo :- ((a; b))::merge.", r1.to_string());
        assert_eq!("foo :- (a, (b, (a; b))).", r2.to_string());

        let c1: Vec<logic::Clause> = (&r1).into();
        assert_eq!(2, c1.len());
        assert_eq!(
            r#"foo :- _operator_merge_begin("0"), a, _operator_merge_end("0")"#,
            c1[0].to_string()
        );
        assert_eq!(
            r#"foo :- _operator_merge_begin("1"), b, _operator_merge_end("1")"#,
            c1[1].to_string()
        );

        let c2: Vec<logic::Clause> = (&r2).into();
        assert_eq!(2, c2.len());
        assert_eq!("foo :- a, b, a", c2[0].to_string());
        assert_eq!("foo :- a, b, b", c2[1].to_string());
    }

    #[test]
    fn modus_constant() {
        // Could use https://crates.io/crates/test_case if this pattern occurs often
        let inp1 = r#""Hello\nWorld""#;
        let inp2 = r#""Tabs\tare\tbetter\tthan\tspaces""#;
        let inp3 = r#""Testing \
                       multiline.""#;
        let (_, s1) = parser::modus_const(Span::new(inp1.into())).unwrap();
        let (_, s2) = parser::modus_const(Span::new(inp2.into())).unwrap();
        let (_, s3) = parser::modus_const(Span::new(inp3.into())).unwrap();

        assert_eq!(s1, r#"Hello\nWorld"#);
        assert_eq!(s2, r#"Tabs\tare\tbetter\tthan\tspaces"#);
        assert_eq!(
            s3,
            r#"Testing \
                       multiline."#
        );
    }

    #[test]
    fn modus_expression() {
        let a: Literal = "a".parse().unwrap();
        let b: Literal = "b".parse().unwrap();
        let c: Literal = "c".parse().unwrap();
        let d: Literal = "d".parse().unwrap();

        let e1 = Expression::And(
            None,
            Expression::Literal(a).into(),
            Expression::Literal(b).into(),
        );
        let e2 = Expression::And(
            None,
            Expression::Literal(c).into(),
            Expression::Literal(d).into(),
        );

        let expr = Expression::Or(None, e1.into(), e2.into());

        let expr_str = "((a, b); (c, d))";
        assert_eq!(expr_str, expr.to_string());

        let rule: ModusClause = format!("foo :- {}.", expr_str).parse().unwrap();
        assert!(expr.eq_ignoring_position(&rule.body.unwrap()))
    }

    #[test]
    fn modus_unification() {
        let inp = "foo(X, Y) :- X = Y.";

        let expected_lit: Literal = "string_eq(X, Y)".parse().unwrap();
        let actual: Expression = inp.parse().map(|r: ModusClause| r.body).unwrap().unwrap();
        assert!(Expression::Literal(expected_lit).eq_ignoring_position(&actual));
    }

    #[test]
    fn multiple_clause_with_different_ops() {
        let foo = Literal {
            position: None,
            predicate: logic::Predicate("foo".into()),
            args: vec![ModusTerm::UserVariable("x".to_owned())],
        };
        let bar = Literal {
            position: None,
            predicate: logic::Predicate("bar".into()),
            args: Vec::new(),
        };
        let baz = Literal {
            position: None,
            predicate: logic::Predicate("baz".into()),
            args: Vec::new(),
        };
        let a = Rule {
            head: logic::Literal {
                position: None,
                predicate: logic::Predicate("a".to_owned()),
                args: vec![],
            },
            body: Some(Expression::And(
                None,
                Box::new(Expression::OperatorApplication(
                    None,
                    Box::new(Expression::And(
                        None,
                        Box::new(foo.into()),
                        Box::new(bar.into()),
                    )),
                    Operator {
                        position: None,
                        predicate: logic::Predicate("setenv".into()),
                        args: vec![
                            ModusTerm::Constant("a".to_owned()),
                            ModusTerm::Constant("foobar".to_owned()),
                        ],
                    },
                )),
                Box::new(Expression::OperatorApplication(
                    None,
                    Box::new(baz.into()),
                    Operator {
                        position: None,
                        predicate: logic::Predicate("setenv".into()),
                        args: vec![
                            ModusTerm::Constant("a".to_owned()),
                            ModusTerm::Constant("baz".to_owned()),
                        ],
                    },
                )),
            )),
        };

        let actual: Rule = r#"a:-(foo(x),bar)::setenv("a","foobar"), (baz)::setenv("a", "baz")."#
            .parse()
            .unwrap();
        assert!(a.eq_ignoring_position(&actual));
    }
}
