// Modus, a language for building container images
// Copyright (C) 2022 University College London

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.

// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! This module contains logical structures that define the intermediate language used by Modus.
//!
//! Currently, these structures are generic, parameterized over the types they may use for constants
//! or variables.

use nom::character::complete::multispace0;
use nom_locate::LocatedSpan;

use crate::analysis::Kind;
use crate::logic::parser::Span;
use crate::sld;
use crate::unification::Rename;

use std::convert::TryInto;
use std::fmt;
use std::fmt::Debug;
use std::ops::Range;
use std::str;
use std::sync::atomic::{AtomicU32, Ordering};
use std::{collections::HashSet, hash::Hash};

impl fmt::Display for IRTerm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IRTerm::Constant(s) => write!(f, "\"{}\"", s),
            IRTerm::UserVariable(s) => write!(f, "{}", s),
            IRTerm::List(ts) => write!(
                f,
                "[{}]",
                ts.iter()
                    .map(|x| x.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            // there may be aux variables after translating to IR
            IRTerm::AuxiliaryVariable(i) => write!(f, "__AUX_{}", i),
            IRTerm::RenamedVariable(i, t) => write!(f, "{}_{}", t, i),
            IRTerm::AnonymousVariable(_) => write!(f, "_"),
        }
    }
}

pub static AVAILABLE_VARIABLE_INDEX: AtomicU32 = AtomicU32::new(0);

impl Rename<IRTerm> for IRTerm {
    fn rename(&self) -> IRTerm {
        match self {
            IRTerm::Constant(_) => (*self).clone(),
            IRTerm::List(ts) => IRTerm::List(ts.iter().map(|t| t.rename()).collect()),
            _ => {
                let index = AVAILABLE_VARIABLE_INDEX.fetch_add(1, Ordering::SeqCst);
                IRTerm::RenamedVariable(index, Box::new((*self).clone()))
            }
        }
    }
}

impl sld::Auxiliary for IRTerm {
    fn aux(anonymous: bool) -> IRTerm {
        let index = AVAILABLE_VARIABLE_INDEX.fetch_add(1, Ordering::SeqCst);
        if anonymous {
            IRTerm::AnonymousVariable(index)
        } else {
            IRTerm::AuxiliaryVariable(index)
        }
    }
}

/// A predicate symbol
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Predicate(pub String);

impl Predicate {
    /// True if this predicate symbol represents an operator.
    pub fn is_operator(&self) -> bool {
        self.0.starts_with("_operator_")
    }

    /// Unmangles the name if it's an operator.
    pub fn unmangle(self) -> Predicate {
        if self.is_operator() {
            Predicate(
                self.0
                    .trim_start_matches("_operator_")
                    .trim_end_matches("_begin")
                    .trim_end_matches("_end")
                    .to_string(),
            )
        } else {
            self
        }
    }

    /// Returns the kind based on this predicate name.
    /// May not be the true kind in an actual Modus program.
    pub fn naive_predicate_kind(&self) -> Kind {
        match self.0.as_str() {
            "from" => Kind::Image,
            "run" | "copy" => Kind::Layer,
            _ => Kind::Logic,
        }
    }
}

impl From<String> for Predicate {
    fn from(s: String) -> Self {
        Predicate(s)
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum IRTerm {
    Constant(String),
    UserVariable(String),
    List(Vec<IRTerm>),

    /// Primarily used to establish f-string constraints.
    AuxiliaryVariable(u32),

    RenamedVariable(u32, Box<IRTerm>),

    /// It should be safe to assume that a given AnonymousVariable(i) will not appear
    /// again in the AST.
    AnonymousVariable(u32),
}

impl IRTerm {
    /// Returns `true` if the IRTerm is [`Constant`].
    ///
    /// [`Constant`]: IRTerm::Constant
    pub fn is_constant(&self) -> bool {
        matches!(self, Self::Constant(..))
    }

    pub fn is_constant_or_compound_constant(&self) -> bool {
        match self {
            Self::Constant(_) => true,
            Self::List(ts) => ts.iter().all(|t| t.is_constant_or_compound_constant()),
            _ => false,
        }
    }

    /// Returns `true` if the IRTerm is [`AnonymousVariable`] or it was
    /// renamed from an [`AnonymousVariable`].
    ///
    /// [`AnonymousVariable`]: IRTerm::AnonymousVariable
    pub fn is_underlying_anonymous_variable(&self) -> bool {
        match self {
            Self::AnonymousVariable(_) => true,
            Self::RenamedVariable(_, t) => t.is_underlying_anonymous_variable(),
            _ => false,
        }
    }

    pub fn as_constant(&self) -> Option<&str> {
        match self {
            IRTerm::Constant(c) => Some(&c[..]),
            _ => None,
        }
    }

    /// Gets the original IRTerm from a renamed one, or returns itself.
    pub fn get_original(&self) -> &IRTerm {
        match self {
            IRTerm::RenamedVariable(_, t) => t.get_original(),
            t => t,
        }
    }

    /// Returns `true` if the irterm is [`AnonymousVariable`].
    ///
    /// [`AnonymousVariable`]: IRTerm::AnonymousVariable
    pub fn is_anonymous_variable(&self) -> bool {
        matches!(self, Self::AnonymousVariable(..))
    }
}

/// Structure that holds information about the position of some section of the source code.
///
/// Not to be confused with `parser::Span`.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct SpannedPosition {
    /// The relative offset of this spanned position from the original input.
    pub offset: usize,

    /// Length of this spanned position. Assumes ASCII text (i.e. each character is a byte).
    pub length: usize,
}

impl From<&SpannedPosition> for Range<usize> {
    fn from(s: &SpannedPosition) -> Self {
        s.offset..(s.offset + s.length)
    }
}

impl From<Span<'_>> for SpannedPosition {
    fn from(s: Span) -> Self {
        SpannedPosition {
            length: s.fragment().len(),
            offset: s.location_offset(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Literal<T = IRTerm> {
    /// True if if this is a positive literal, else it's a negated literal.
    /// Double negations should be collapsed, if any.
    pub positive: bool,

    pub position: Option<SpannedPosition>,
    pub predicate: Predicate,
    pub args: Vec<T>,
}

#[cfg(test)]
impl<T: PartialEq> Literal<T> {
    /// Checks for equality, ignoring the position fields.
    pub fn eq_ignoring_position(&self, other: &Literal<T>) -> bool {
        self.positive == other.positive
            && self.predicate == other.predicate
            && self.args == other.args
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Signature(pub Predicate, pub u32);

#[derive(Clone, PartialEq, Debug)]
pub struct Clause<T = IRTerm> {
    pub head: Literal<T>,
    pub body: Vec<Literal<T>>,
}

#[cfg(test)]
impl<T: PartialEq> Clause<T> {
    pub fn eq_ignoring_position(&self, other: &Clause<T>) -> bool {
        self.head.eq_ignoring_position(&other.head)
            && self.body.len() == other.body.len()
            && self
                .body
                .iter()
                .enumerate()
                .all(|(i, l)| l.eq_ignoring_position(&other.body[i]))
    }
}

pub trait Ground {
    fn is_ground(&self) -> bool;
}

impl IRTerm {
    pub fn variables(&self, include_anonymous: bool) -> HashSet<IRTerm> {
        let mut set = HashSet::<IRTerm>::new();
        match (self, include_anonymous) {
            (IRTerm::AnonymousVariable(_), true) => {
                set.insert(self.clone());
            }
            (IRTerm::List(ts), b) => {
                set.extend(ts.iter().flat_map(|t| t.variables(b)));
            }
            (IRTerm::AuxiliaryVariable(_), _)
            | (IRTerm::RenamedVariable(..), _)
            | (IRTerm::UserVariable(_), _) => {
                set.insert(self.clone());
            }
            (IRTerm::Constant(_), _) | (IRTerm::AnonymousVariable(_), false) => (),
        }
        set
    }
}

impl Literal {
    pub fn signature(&self) -> Signature {
        Signature(self.predicate.clone(), self.args.len().try_into().unwrap())
    }
    pub fn variables(&self, include_anonymous: bool) -> HashSet<IRTerm> {
        self.args
            .iter()
            .map(|r| r.variables(include_anonymous))
            .reduce(|mut l, r| {
                l.extend(r);
                l
            })
            .unwrap_or_default()
    }

    /// Unmangles this literal if it represents an operator. Replaces it's predicate name
    /// and removes argument used for matching the operator.
    pub fn unmangle(self) -> Literal {
        if self.predicate.is_operator() {
            Literal {
                predicate: self.predicate.unmangle(),
                args: self.args[1..].to_vec(),
                ..self
            }
        } else {
            self
        }
    }

    pub fn negated(&self) -> Literal {
        Literal {
            positive: !self.positive,
            ..self.clone()
        }
    }

    /// Returns a copy of the literal with the terms 'normalized'.
    /// Currently this means just getting the original terms for any renamed terms.
    pub fn normalized_terms(self) -> Literal {
        Literal {
            args: self.args.iter().map(|t| t.get_original().clone()).collect(),
            ..self
        }
    }
}

impl<T> Literal<T> {
    pub fn with_position(self, position: Option<SpannedPosition>) -> Literal<T> {
        Literal { position, ..self }
    }
}

impl Clause {
    pub fn variables(&self, include_anonymous: bool) -> HashSet<IRTerm> {
        let mut body = self
            .body
            .iter()
            .map(|r| r.variables(include_anonymous))
            .reduce(|mut l, r| {
                l.extend(r);
                l
            })
            .unwrap_or_default();
        body.extend(self.head.variables(include_anonymous));
        body
    }
}

impl Ground for IRTerm {
    fn is_ground(&self) -> bool {
        matches!(self, IRTerm::Constant(_))
    }
}

impl Ground for Literal {
    fn is_ground(&self) -> bool {
        self.variables(true).is_empty()
    }
}

impl Ground for Clause {
    fn is_ground(&self) -> bool {
        self.variables(true).is_empty()
    }
}

impl fmt::Display for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.0, self.1)
    }
}

impl fmt::Display for Predicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

fn display_sep<T: fmt::Display>(seq: &[T], sep: &str) -> String {
    return seq
        .iter()
        .map(|t| t.to_string())
        .collect::<Vec<String>>()
        .join(sep);
}

impl fmt::Display for Literal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &*self.args {
            [] => write!(
                f,
                "{}{}",
                if self.positive { "" } else { "!" },
                self.predicate
            ),
            _ => write!(
                f,
                "{}{}({})",
                if self.positive { "" } else { "!" },
                self.predicate,
                display_sep(&self.args, ", ")
            ),
        }
    }
}

impl fmt::Display for Clause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} :- {}", self.head, display_sep(&self.body, ", "))
    }
}

impl str::FromStr for Clause {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let span = Span::new(s);
        match parser::clause(parser::term)(span) {
            Result::Ok((_, o)) => Ok(o),
            Result::Err(e) => Result::Err(format!("{}", e)),
        }
    }
}

impl str::FromStr for Literal {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let span = Span::new(s);
        match parser::literal(parser::term, multispace0)(span) {
            Result::Ok((_, o)) => Ok(o),
            Result::Err(e) => Result::Err(format!("{}", e)),
        }
    }
}

/// The parser for the IR is only for convenience in writing tests.
pub mod parser {

    use super::*;

    use nom::{
        branch::alt,
        bytes::complete::{is_a, take_until},
        character::complete::{alpha1, alphanumeric1, multispace0},
        combinator::{cut, map, opt, recognize},
        multi::{many0, many0_count, separated_list0, separated_list1},
        sequence::{delimited, pair, preceded, terminated, tuple},
        Offset, Slice,
    };
    use nom_supreme::{error::ErrorTree, tag::complete::tag};

    pub type Span<'a> = LocatedSpan<&'a str>;

    /// Redeclaration that uses ErrorTree instead of the default nom::Error.
    pub type IResult<T, O> = nom::IResult<T, O, ErrorTree<T>>;

    /// Creates a parser that returns a `SpannedPosition` that spans the consumed input
    /// of a given parser. Also returns the actual output of the parser.
    pub fn recognized_span<'a, P, T>(
        mut inner: P,
    ) -> impl FnMut(Span<'a>) -> IResult<Span<'a>, (SpannedPosition, T)>
    where
        P: FnMut(Span<'a>) -> IResult<Span<'a>, T>,
    {
        move |i| {
            let original_i = i.clone();

            let (i, o) = inner(i)?;

            let index = original_i.offset(&i);
            let recognized_section = original_i.slice(..index);
            let spanned_pos: SpannedPosition = recognized_section.into();

            Ok((i, (spanned_pos, o)))
        }
    }

    fn ws<'a, F: 'a, O>(inner: F) -> impl FnMut(Span<'a>) -> IResult<Span<'a>, O>
    where
        F: FnMut(Span<'a>) -> IResult<Span<'a>, O>,
    {
        delimited(multispace0, inner, multispace0)
    }

    fn constant(i: Span) -> IResult<Span, Span> {
        delimited(tag("\""), take_until("\""), tag("\""))(i)
    }

    fn variable(i: Span) -> IResult<Span, Span> {
        literal_identifier(i)
    }

    fn list_term(i: Span) -> IResult<Span, Vec<IRTerm>> {
        delimited(
            terminated(tag("["), multispace0),
            separated_list0(delimited(multispace0, tag(","), multispace0), term),
            preceded(multispace0, tag("]")),
        )(i)
    }

    pub fn term(i: Span) -> IResult<Span, IRTerm> {
        alt((
            map(list_term, IRTerm::List),
            map(constant, |s| IRTerm::Constant(s.fragment().to_string())),
            map(is_a("_"), |_| sld::Auxiliary::aux(true)),
            map(variable, |s| IRTerm::UserVariable(s.fragment().to_string())),
        ))(i)
    }

    //TODO: I need to think more carefully how to connect this to stage name
    pub fn literal_identifier(i: Span) -> IResult<Span, Span> {
        recognize(pair(
            alt((alpha1, tag("_"))),
            many0(alt((alphanumeric1, tag("_"), tag("-")))),
        ))(i)
    }

    /// Parses a literal with a generic term type.
    pub fn literal<'a, FT: 'a, T, S, Any>(
        term: FT,
        space0: S,
    ) -> impl FnMut(Span<'a>) -> IResult<Span<'a>, Literal<T>>
    where
        FT: FnMut(Span<'a>) -> IResult<Span<'a>, T> + Clone,
        S: FnMut(Span<'a>) -> IResult<Span<'a>, Any> + Clone,
    {
        move |i| {
            let (i, (spanned_pos, (neg_count, name, args))) = recognized_span(tuple((
                // allow whitespace between '!'
                many0_count(terminated(
                    nom::character::complete::char('!'),
                    space0.clone(),
                )),
                terminated(literal_identifier, space0.clone()),
                opt(delimited(
                    terminated(tag("("), space0.clone()),
                    separated_list1(
                        terminated(tag(","), space0.clone()),
                        terminated(term.clone(), space0.clone()),
                    ),
                    cut(terminated(tag(")"), space0.clone())),
                )),
            )))(i)?;

            Ok((
                i,
                Literal {
                    positive: neg_count % 2 == 0,
                    position: Some(spanned_pos),
                    predicate: Predicate(name.fragment().to_string()),
                    args: match args {
                        Some(args) => args,
                        None => Vec::new(),
                    },
                },
            ))
        }
    }

    pub fn clause<'a, FT: 'a, T>(term: FT) -> impl FnMut(Span<'a>) -> IResult<Span<'a>, Clause<T>>
    where
        FT: FnMut(Span) -> IResult<Span, T> + Clone,
    {
        map(
            pair(
                literal(term.clone(), multispace0),
                opt(preceded(
                    ws(tag(":-")),
                    separated_list0(ws(tag(",")), literal(term, multispace0)),
                )),
            ),
            |(head, body)| Clause {
                head,
                body: body.unwrap_or(Vec::new()),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_term() {
        let inp = "\"\"";

        let expected = IRTerm::Constant("".into());
        let actual: IRTerm = parser::term(Span::new(inp)).unwrap().1;

        assert_eq!(expected, actual);
    }

    #[test]
    fn literals() {
        let l1 = Literal {
            positive: true,
            position: None,
            predicate: Predicate("l1".into()),
            args: vec![IRTerm::Constant("c".into()), IRTerm::Constant("d".into())],
        };

        assert_eq!("l1(\"c\", \"d\")", l1.to_string());

        let actual1: Literal = "l1(\"c\", \"d\")".parse().unwrap();
        let actual2: Literal = "l1(\"c\",\n\t\"d\")".parse().unwrap();
        assert!(l1.eq_ignoring_position(&actual1));
        assert!(l1.eq_ignoring_position(&actual2));
    }

    #[test]
    fn literal_with_variable() {
        let l1 = Literal {
            positive: true,
            position: None,
            predicate: Predicate("l1".into()),
            args: vec![
                IRTerm::Constant("".into()),
                IRTerm::UserVariable("X".into()),
            ],
        };

        assert_eq!("l1(\"\", X)", l1.to_string());

        let actual: Literal = "l1(\"\", X)".parse().unwrap();
        assert!(l1.eq_ignoring_position(&actual));
    }

    #[test]
    fn negated_literal() {
        let l1 = Literal {
            positive: false,
            position: None,
            predicate: Predicate("l1".into()),
            args: vec![
                IRTerm::Constant("".into()),
                IRTerm::UserVariable("X".into()),
            ],
        };

        assert_eq!("!l1(\"\", X)", l1.to_string());

        let actual: Literal = "!!!l1(\"\", X)".parse().unwrap();
        assert!(l1.eq_ignoring_position(&actual));
    }

    #[test]
    fn span_of_literal() {
        let spanned_pos = SpannedPosition {
            length: 22,
            offset: 0,
        };

        let actual: Literal = "l1(\"test_constant\", X)".parse().unwrap();
        assert_eq!(Some(spanned_pos), actual.position);
    }

    #[test]
    fn simple_rule() {
        let c = IRTerm::Constant("c".into());
        let va = IRTerm::UserVariable("A".into());
        let vb = IRTerm::UserVariable("B".into());
        let l1 = Literal {
            positive: true,
            position: None,
            predicate: Predicate("l1".into()),
            args: vec![va.clone(), vb.clone()],
        };
        let l2 = Literal {
            positive: true,
            position: None,
            predicate: Predicate("l2".into()),
            args: vec![va.clone(), c.clone()],
        };
        let l3 = Literal {
            positive: true,
            position: None,
            predicate: Predicate("l3".into()),
            args: vec![vb.clone(), c.clone()],
        };
        let r = Clause {
            head: l1,
            body: vec![l2, l3],
        };

        assert_eq!("l1(A, B) :- l2(A, \"c\"), l3(B, \"c\")", r.to_string());

        let actual: Clause = "l1(A, B) :- l2(A, \"c\"), l3(B, \"c\")".parse().unwrap();
        assert!(r.eq_ignoring_position(&actual));
    }

    #[test]
    fn nullary_predicate() {
        let va = IRTerm::UserVariable("A".into());
        let l1 = Literal {
            positive: true,
            position: None,
            predicate: Predicate("l1".into()),
            args: Vec::new(),
        };
        let l2 = Literal {
            positive: true,
            position: None,
            predicate: Predicate("l2".into()),
            args: vec![va.clone()],
        };
        let r = Clause {
            head: l1,
            body: vec![l2],
        };

        assert_eq!("l1 :- l2(A)", r.to_string());

        let actual: Clause = "l1 :- l2(A)".parse().unwrap();
        assert!(r.eq_ignoring_position(&actual))
    }
}
