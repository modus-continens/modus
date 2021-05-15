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


use std::{collections::HashMap, hash::Hash};

use crate::logic;
use logic::{ Atom, Term, Rule, Literal, Groundness };

type Substitution<C, V> = HashMap<V, Term<C, V>>;

impl<C, V> Groundness for Substitution<C, V> {
    fn is_grounded() -> bool {
        todo!()
    }
}

trait Substitutable<C, V> {
    type Output;

    fn substitute(&self, s: &Substitution<C, V>) -> Self::Output;
} 

impl<C: Clone, V: Eq + Hash + Clone> Substitutable<C, V> for Term<C, V> {
    type Output = Term<C, V>;
    fn substitute(&self, s: &Substitution<C, V>) -> Self::Output {
        match &self {
            Term::Variable(v) => s.get(v).unwrap_or(self).clone(),
            Term::Compound(atom, args) => todo!(),
            _ => self.clone()
        }
    }
}

impl<C: Clone, V: Eq + Hash + Clone> Substitutable<C, V> for Literal<C, V> {
    type Output = Literal<C, V>;
    fn substitute(&self, s: &Substitution<C, V>) -> Self::Output {
        Literal { atom: self.atom.clone(), args: self.args.iter().map(|t| t.substitute(s)).collect() }
    }
}

impl<C: Clone, V: Eq + Hash + Clone> Substitutable<C, V> for Vec<Literal<C, V>> {
    type Output = Vec<Literal<C, V>>;
    fn substitute(&self, s: &Substitution<C, V>) -> Self::Output {
        self.iter().map(|l| l.substitute(s)).collect()
    }
}

fn composition<C: Clone, V: Eq + Hash + Clone>(l: &Substitution<C, V>, r: &Substitution<C, V>) -> Substitution<C, V> {
    let mut result = HashMap::<V, Term<C, V>>::new();
    for (k, v) in l {
        result.insert(k.clone(), v.substitute(r));
    }
    for (k, v) in r {
        result.insert(k.clone(), v.clone());
    }
    result
}

impl<C, V> Literal<C, V>
where
    C: PartialEq + Clone,
    V: PartialEq + Eq + Hash + Clone
{
    pub fn unify(&self, other: &Literal<C, V>) -> Option<Substitution<C, V>> {
        if self.signature() != other.signature() {
            return None;
        }
        let mut s = HashMap::<V, Term<C, V>>::new();
        for (i, self_term) in self.args.iter().enumerate() {
            let other_term = &other.args[i];
            match (self_term, other_term) {
                (Term::Compound(_, _), _) | (_, Term::Compound(_, _)) => 
                    panic!("compound terms are not supported in unification"),
                _ => ()
            }
            let self_term_subs = self_term.substitute(&s);
            let other_term_subs = other_term.substitute(&s);
            if self_term_subs != other_term_subs {
                match self_term_subs {
                    Term::Variable(v) => {
                        let mut upd = HashMap::<V, Term<C, V>>::new();
                        upd.insert(v.clone(), other_term_subs.clone());
                        s = composition(&s, &upd);
                    },
                    _ => match other_term_subs {
                        Term::Variable(v) => {
                            let mut upd = HashMap::<V, Term<C, V>>::new();
                            upd.insert(v.clone(), self_term_subs.clone());
                            s = composition(&s, &upd);
                        },
                        _ => return None
                    }
                }
            }
        }
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn simple_unifier() {
        let l: logic::toy::Literal = "a(X, c)".parse().unwrap();
        let m: logic::toy::Literal = "a(d, Y)".parse().unwrap();
        let result = l.unify(&m);
        assert!(result.is_some());
        let mgu = result.unwrap();
        assert_eq!(l.substitute(&mgu), m.substitute(&mgu))
    }

    #[test]
    fn complex_unifier() {
        let l: logic::toy::Literal = "p(Y, Y, V, W)".parse().unwrap();
        let m: logic::toy::Literal = "p(X, Z, a, U)".parse().unwrap();
        let result = l.unify(&m);
        assert!(result.is_some());
        let mgu = result.unwrap();      
        assert_eq!(l.substitute(&mgu), m.substitute(&mgu));
        assert_eq!(mgu.get("Y".into()), Some(&Term::Variable("Z".into())));
        assert_eq!(mgu.get("X".into()), Some(&Term::Variable("Z".into())));
        assert_eq!(mgu.get("V".into()), Some(&Term::Constant(Atom("a".into()))));
        assert_eq!(mgu.get("W".into()), Some(&Term::Variable("U".into())));
    }

    #[test]
    fn simple_non_unifiable() {
        let l: logic::toy::Literal = "a(X, b)".parse().unwrap();
        let m: logic::toy::Literal = "a(Y)".parse().unwrap();
        let result = l.unify(&m);
        assert!(result.is_none());
    }

    #[test]
    fn complex_non_unifiable() {
        let l: logic::toy::Literal = "q(X, a, X, b)".parse().unwrap();
        let m: logic::toy::Literal = "q(Y, a, a, Y)".parse().unwrap();
        let result = l.unify(&m);
        assert!(result.is_none());
    }
}