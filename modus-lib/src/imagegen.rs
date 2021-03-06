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

use std::collections::{HashMap, HashSet};
use std::iter::{self, FromIterator};
use std::path::{Path, PathBuf};

use crate::analysis::{Kind, ModusSemantics};
use crate::logic::{Clause, IRTerm, Literal, Predicate};
use crate::modusfile::{self, Modusfile};
use crate::sld::{self, ClauseId, Proof, ResolutionError};
use crate::translate::translate_modusfile;
use crate::unification::Substitute;

use codespan_reporting::diagnostic::Diagnostic;
use serde::{Deserialize, Serialize};

const MODUS_LABEL: &str = "com.modus-continens.literal";

/// A build plan, designed to be easy to translate to buildkit and Dockerfile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildPlan {
    pub nodes: Vec<BuildNode>,
    pub dependencies: Vec<Vec<NodeId>>,
    pub outputs: Vec<Output>,
}

impl BuildPlan {
    pub fn new() -> BuildPlan {
        BuildPlan {
            nodes: Vec::new(),
            dependencies: Vec::new(),
            outputs: Vec::new(),
        }
    }

    pub fn new_node(&mut self, node: BuildNode, deps: Vec<NodeId>) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(node);
        self.dependencies.push(
            HashSet::<_>::from_iter(deps.into_iter())
                .into_iter()
                .collect(),
        );
        debug_assert_eq!(self.nodes.len(), self.dependencies.len());
        id
    }

    /// Return an ordering of nodes in which dependencies of a node comes before
    /// the node itself.
    pub fn topological_order(&self) -> Vec<NodeId> {
        let mut topological_order = Vec::with_capacity(self.nodes.len());
        let mut seen = vec![false; self.nodes.len()];
        fn dfs(
            plan: &BuildPlan,
            node: NodeId,
            topological_order: &mut Vec<NodeId>,
            seen: &mut Vec<bool>,
        ) {
            if seen[node] {
                return;
            }
            for &deps in plan.dependencies[node].iter() {
                dfs(plan, deps, topological_order, seen);
            }
            topological_order.push(node);
            seen[node] = true;
        }
        for output in self.outputs.iter() {
            dfs(&self, output.node, &mut topological_order, &mut seen);
        }
        topological_order
    }
}

#[derive(Debug)]
struct State {
    current_node: Option<NodeId>,
    cwd: String,
    current_merge: Option<MergeNode>,
    additional_envs: HashMap<String, String>,
}

impl State {
    fn with_new_cwd<F: FnOnce(&mut Self)>(&mut self, new_cwd: String, f: F) {
        let old_cwd = std::mem::replace(&mut self.cwd, new_cwd);
        f(self);
        self.cwd = old_cwd;
    }

    fn with_new_merge<F: FnOnce(&mut Self)>(&mut self, new_merge: MergeNode, f: F) -> MergeNode {
        debug_assert!(self.current_merge.is_none());
        self.current_merge = Some(new_merge);
        f(self);
        self.current_merge.take().unwrap()
    }

    fn has_base(&self) -> bool {
        self.current_merge.is_some() || self.current_node.is_some()
    }

    fn set_node(&mut self, node: NodeId) {
        debug_assert!(self.current_merge.is_none());
        self.current_node = Some(node);
    }

    fn with_additional_envs<E: IntoIterator<Item = (String, String)>, F: FnOnce(&mut Self)>(
        &mut self,
        envs: E,
        f: F,
    ) {
        let old_envs = self.additional_envs.clone();
        self.additional_envs.extend(envs);
        f(self);
        self.additional_envs = old_envs;
    }
}

pub type NodeId = usize;

/// Represent one operation, such as `RUN` or `FROM`.
///
/// Think of it as one line of a Dockerfile, or one node in the buildkit graph.
///
/// ## Paths
///
/// All the paths in this structure can either be absolute or relative path. In
/// the case of relative paths, they are ALWAYS relative to the working
/// directory of the parent image (as stored in the image config). Translators
/// from this to e.g. buildkit LLB should resolve the paths as necessary.
///
/// In the case of copy, src_path and dst_path should be resolved relative to
/// the source image's workdir and the destination (parent) image's workdir,
/// respectively.
///
/// TODO: add caching control
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BuildNode {
    From {
        /// The actual image reference to use. Probably a resolved hash.
        image_ref: String,
        /// What user specified initially, such as "alpine".
        display_name: String,
    },
    FromScratch {
        /// A hack, inserted by buildkit.rs See buildkit_frontend.rs for documentation
        scratch_ref: Option<String>,
    },
    Run {
        parent: NodeId,
        command: String,
        cwd: String,
        additional_envs: HashMap<String, String>,
    },
    CopyFromImage {
        parent: NodeId,
        src_image: NodeId,
        src_path: String,
        dst_path: String,
    },
    CopyFromLocal {
        parent: NodeId,
        src_path: String,
        dst_path: String,
    },
    SetWorkdir {
        parent: NodeId,
        new_workdir: String,
    },
    SetEntrypoint {
        parent: NodeId,
        new_entrypoint: Vec<String>,
    },
    SetCmd {
        parent: NodeId,
        new_cmd: Vec<String>,
    },
    SetLabel {
        parent: NodeId,
        label: String,
        value: String,
    },
    Merge(MergeNode),
    SetEnv {
        parent: NodeId,
        key: String,
        value: String,
    },
    AppendEnvValue {
        parent: NodeId,
        key: String,
        value: String,
    },
    SetUser {
        parent: NodeId,
        user: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeNode {
    pub parent: NodeId,
    pub operations: Vec<MergeOperation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MergeOperation {
    Run {
        command: String,
        cwd: String,
        additional_envs: HashMap<String, String>,
    },
    CopyFromImage {
        src_image: NodeId,
        src_path: String,
        dst_path: String,
    },
    CopyFromLocal {
        src_path: String,
        dst_path: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Output {
    pub node: NodeId,
    #[serde(skip)]
    pub source_literal: Option<Literal>,
}

/// Given a list of pairs of ground (solved) queries and their proof tree, output
/// a build graph which builds all the queried images.
pub fn build_dag_from_proofs(
    query_and_proofs: &[(Literal, Proof)],
    rules: &Vec<Clause<IRTerm>>,
) -> BuildPlan {
    let mut res = BuildPlan::new();
    let mut image_literals: HashMap<Literal, NodeId> = HashMap::new();

    /// Takes in a part of the build tree, assuming that it is building an image
    /// (for example, the tree of an image literal, or a slice of a bigger tree,
    /// where the slice contains literals that occurs between _operator_copy_begin
    /// and _operator_copy_end).
    ///
    /// This function should return None if the subtree passed in does not build
    /// an image, for example because there is no call to any intrinsic. It
    /// should, however, report an error if there is no `from` but there is a
    /// `run`.
    fn process_image(
        subtree: &[&Proof],
        rules: &Vec<Clause<IRTerm>>,
        res: &mut BuildPlan,
        image_literals: &mut HashMap<Literal, NodeId>,
        tag_with_literal: Option<String>,
    ) -> Option<NodeId> {
        let mut curr_state = State {
            current_node: None,
            cwd: "".to_string(),
            current_merge: None,
            additional_envs: HashMap::new(),
        };

        /* We go through the proof tree in depth-first order, since this is
         * effectively what the build instructions are supposed to be ordered in.
         *
         * Consider this as an example:
         * a :- b, c, run(5)
         * b :- run(1)
         * c :- run(2), d, run(4)
         * d :- run(3)
         *
         * Remember that repeated call to a literal will result in the expansion
         * of that literal to be repeated in the tree, so we don't need to worry
         * about that. A special optimization is made for when we see a literal
         * before any `from` (last_node == None), in which case we can check if
         * we already built that literal as another image, and reuse when
         * possible. In the case where we haven't, we can also store the node we
         * built from going into that literal into the image_literals store, to
         * be re-used later.
         */
        fn process_tree(
            proof: &Proof,
            rules: &Vec<Clause<IRTerm>>,
            res: &mut BuildPlan,
            image_literals: &mut HashMap<Literal, NodeId>,
            curr_state: &mut State,
        ) {
            match proof.clause {
                ClauseId::Query => {}
                ClauseId::Builtin(ref intrinsic) => {
                    process_intrinsic(intrinsic, res, image_literals, curr_state);
                    debug_assert!(proof.children.is_empty()); // Intrinsics should not have children.
                    return;
                }
                ClauseId::Rule(rid) => {
                    let substituted_lit = rules[rid].head.substitute(&proof.valuation);
                    debug_assert!(substituted_lit
                        .args
                        .iter()
                        .all(|x| x.is_constant_or_compound_constant()));

                    if !curr_state.has_base() {
                        // Do the optimization mentioned above.
                        if let Some(&node_id) = image_literals.get(&substituted_lit) {
                            curr_state.set_node(node_id);
                            return; // no need to recurse to children anymore.
                        } else {
                            if let Some(node_id) = process_image(
                                &proof.children.iter().collect::<Vec<_>>()[..],
                                rules,
                                res,
                                image_literals,
                                Some(substituted_lit.to_string()),
                            ) {
                                curr_state.set_node(node_id);
                                image_literals.insert(substituted_lit, node_id);
                                return; // no need to recurse to children anymore, since I just built the content of this literal.
                            } else {
                                return; // the literal doesn't do any docker thing, so we can safely skip it.
                            }
                        }
                    } else {
                        // Can't re-use anymore since we already started an image.
                        // In this case the subtree of this literal shouldn't be
                        // an image anyway, so just dfs as normal.
                    }
                }
                ClauseId::NegationCheck(_) => {}
            }

            process_children(
                &proof.children.iter().collect::<Vec<_>>(),
                rules,
                res,
                image_literals,
                curr_state,
            );
        }

        fn process_intrinsic(
            intrinsic: &Literal,
            res: &mut BuildPlan,
            image_literals: &mut HashMap<Literal, NodeId>,
            curr_state: &mut State,
        ) {
            let name = &intrinsic.predicate.0[..];
            assert!(!name.starts_with("_operator_")); // operators handled separately below.
            match name {
                "from" => {
                    if curr_state.current_merge.is_some() {
                        panic!("You can not generate a new image inside a merge.");
                    }
                    if curr_state.has_base() {
                        panic!("from must be the first build instruction.");
                    }
                    // Special sharing for the "from" intrinsic.
                    if let Some(&existing_node) = image_literals.get(&intrinsic) {
                        curr_state.set_node(existing_node);
                    } else {
                        let image_ref = intrinsic.args[0].as_constant().unwrap().to_owned();
                        let new_node;
                        if &image_ref == "scratch" {
                            new_node =
                                res.new_node(BuildNode::FromScratch { scratch_ref: None }, vec![]);
                        } else {
                            new_node = res.new_node(
                                BuildNode::From {
                                    display_name: image_ref.clone(),
                                    image_ref,
                                },
                                vec![],
                            );
                        }
                        curr_state.set_node(new_node);
                        image_literals.insert(intrinsic.clone(), new_node);
                    }
                }
                "run" => {
                    let command = intrinsic.args[0].as_constant().unwrap().to_owned();
                    if let Some(ref mut curr_merge) = curr_state.current_merge {
                        curr_merge.operations.push(MergeOperation::Run {
                            command,
                            cwd: curr_state.cwd.clone(),
                            additional_envs: curr_state.additional_envs.clone(),
                        });
                    } else {
                        if !curr_state.has_base() {
                            panic!("No base layer yet.");
                        }
                        let parent = curr_state.current_node.unwrap();
                        curr_state.set_node(res.new_node(
                            BuildNode::Run {
                                parent: parent,
                                command: command,
                                cwd: curr_state.cwd.clone(),
                                additional_envs: curr_state.additional_envs.clone(),
                            },
                            vec![parent],
                        ));
                    }
                }
                "copy" => {
                    let src_path = intrinsic.args[0].as_constant().unwrap().to_owned();
                    if src_path.starts_with("/") {
                        panic!("The source of a local copy can not be an absolute path.");
                    }
                    let dst_path = intrinsic.args[1].as_constant().unwrap();
                    let dst_path = join_path(&curr_state.cwd, dst_path);
                    if let Some(ref mut curr_merge) = curr_state.current_merge {
                        curr_merge
                            .operations
                            .push(MergeOperation::CopyFromLocal { src_path, dst_path });
                    } else {
                        if !curr_state.has_base() {
                            panic!("No base layer yet.");
                        }
                        let parent = curr_state.current_node.unwrap();
                        curr_state.set_node(res.new_node(
                            BuildNode::CopyFromLocal {
                                parent,
                                src_path,
                                dst_path,
                            },
                            vec![parent],
                        ));
                    }
                }
                _ => {
                    // do nothing - there might be stuff like string_concat.
                }
            }
        }

        fn process_operator(
            subtree_in_op: &[&Proof],
            op_name: &str,
            lit: &Literal, // the "begin" literal of the operator.
            rules: &Vec<Clause<IRTerm>>,
            res: &mut BuildPlan,
            image_literals: &mut HashMap<Literal, NodeId>,
            curr_state: &mut State,
        ) {
            match op_name {
                // Image-to-image copy. (local copy is not an operator)
                "copy" => {
                    let src_image = process_image(subtree_in_op, rules, res, image_literals, None)
                        .expect("Stuff inside this copy does not build an image.");
                    let src_path = lit.args[1].as_constant().unwrap().to_owned();
                    let dst_path = join_path(&curr_state.cwd, lit.args[2].as_constant().unwrap());
                    if let Some(ref mut curr_merge) = curr_state.current_merge {
                        curr_merge.operations.push(MergeOperation::CopyFromImage {
                            src_image,
                            src_path,
                            dst_path,
                        });
                    } else {
                        let parent = curr_state.current_node.expect("No base layer yet.");
                        let node = res.new_node(
                            BuildNode::CopyFromImage {
                                parent,
                                src_image,
                                src_path,
                                dst_path,
                            },
                            vec![parent, src_image],
                        );
                        curr_state.set_node(node);
                    }
                }
                "in_workdir" => {
                    let new_p = lit.args[1].as_constant().unwrap();
                    let new_cwd = join_path(&curr_state.cwd, new_p);
                    curr_state.with_new_cwd(new_cwd, |new_state| {
                        process_children(subtree_in_op, rules, res, image_literals, new_state);
                    });
                    // TODO: emit a warning if the tree inside attempts
                    // to build a fresh image - this is probably an incorrect usage.
                }
                "set_workdir" | "set_entrypoint" | "set_cmd" | "set_env" | "append_path"
                | "set_label" | "set_user" => {
                    if curr_state.current_merge.is_some() {
                        panic!("You can not generate a new image inside a merge.");
                    }
                    let img = process_image(subtree_in_op, rules, res, image_literals, None)
                        .expect(&format!("{} should be applied to an image.", op_name));
                    if curr_state.has_base() {
                        panic!(
                            "{} generates a new image, so it should be the first instruction.",
                            op_name
                        );
                    }

                    match op_name {
                        "set_workdir" => {
                            let new_p = lit.args[1].as_constant().unwrap();
                            curr_state.set_node(res.new_node(
                                BuildNode::SetWorkdir {
                                    parent: img,
                                    new_workdir: join_path(&curr_state.cwd, new_p),
                                },
                                vec![img],
                            ));
                        }
                        "set_entrypoint" => {
                            let arg = &lit.args[1];
                            let entrypoint = match arg {
                                IRTerm::Constant(c) => vec![c.to_owned()],
                                IRTerm::List(ts) => ts
                                    .iter()
                                    .map(|t| t.as_constant().unwrap().to_owned())
                                    .collect(),
                                _ => unreachable!(),
                            };
                            curr_state.set_node(res.new_node(
                                BuildNode::SetEntrypoint {
                                    parent: img,
                                    new_entrypoint: entrypoint,
                                },
                                vec![img],
                            ));
                        }
                        "set_cmd" => {
                            let arg = &lit.args[1];
                            let cmd = match arg {
                                IRTerm::List(ts) => ts
                                    .iter()
                                    .map(|t| t.as_constant().unwrap().to_owned())
                                    .collect::<Vec<_>>(),
                                IRTerm::Constant(c) => vec![c.to_owned()],
                                _ => unreachable!(),
                            };
                            curr_state.set_node(res.new_node(
                                BuildNode::SetCmd {
                                    parent: img,
                                    new_cmd: cmd,
                                },
                                vec![img],
                            ));
                        }
                        "set_env" => {
                            let env_k = lit.args[1].as_constant().unwrap().to_owned();
                            let env_v = lit.args[2].as_constant().unwrap().to_owned();
                            curr_state.set_node(res.new_node(
                                BuildNode::SetEnv {
                                    parent: img,
                                    key: env_k,
                                    value: env_v,
                                },
                                vec![img],
                            ));
                        }
                        "append_path" => {
                            let append = format!(":{}", lit.args[1].as_constant().unwrap());
                            curr_state.set_node(res.new_node(
                                BuildNode::AppendEnvValue {
                                    parent: img,
                                    key: "PATH".to_owned(),
                                    value: append,
                                },
                                vec![img],
                            ));
                        }
                        "set_label" => {
                            let label_k = lit.args[1].as_constant().unwrap().to_owned();
                            let label_v = lit.args[2].as_constant().unwrap().to_owned();
                            curr_state.set_node(res.new_node(
                                BuildNode::SetLabel {
                                    parent: img,
                                    label: label_k,
                                    value: label_v,
                                },
                                vec![img],
                            ));
                        }
                        "set_user" => {
                            let user = lit.args[1].as_constant().unwrap().to_owned();
                            curr_state.set_node(
                                res.new_node(BuildNode::SetUser { parent: img, user }, vec![img]),
                            );
                        }
                        _ => unreachable!(),
                    }
                }
                "merge" => {
                    if curr_state.current_merge.is_some() {
                        process_children(subtree_in_op, rules, res, image_literals, curr_state);
                        return;
                    }
                    if !curr_state.has_base() {
                        panic!("merge requires a base layer outside.");
                    }
                    let parent = curr_state.current_node.unwrap();
                    let merge_node = MergeNode {
                        parent,
                        operations: vec![],
                    };
                    let merge_node = curr_state.with_new_merge(merge_node, |new_state| {
                        process_children(subtree_in_op, rules, res, image_literals, new_state);
                    });
                    let mut deps: Vec<NodeId> = merge_node
                        .operations
                        .iter()
                        .filter_map(|x| match x {
                            MergeOperation::CopyFromImage { src_image, .. } => Some(*src_image),
                            // Explicitly list out all the no-dependency cases to prevent future errors.
                            MergeOperation::CopyFromLocal { .. } | MergeOperation::Run { .. } => {
                                None
                            }
                        })
                        .collect();
                    deps.push(parent);
                    curr_state.set_node(res.new_node(BuildNode::Merge(merge_node), deps));
                }
                "in_env" => {
                    let env_k = lit.args[1].as_constant().unwrap().to_owned();
                    let env_v = lit.args[2].as_constant().unwrap().to_owned();
                    curr_state.with_additional_envs([(env_k, env_v)], |new_state| {
                        process_children(subtree_in_op, rules, res, image_literals, new_state);
                    });
                }
                _ => {
                    panic!("Unkown operator: {}", op_name);
                }
            }
        }

        fn process_children(
            children: &[&Proof],
            rules: &Vec<Clause<IRTerm>>,
            res: &mut BuildPlan,
            image_literals: &mut HashMap<Literal, NodeId>,
            curr_state: &mut State,
        ) {
            let mut i = 0usize;
            while i < children.len() {
                let child = children[i];
                if let ClauseId::Builtin(ref lit) = child.clause {
                    let name = &lit.predicate.0;
                    if let Some(op_name) = name
                        .strip_prefix("_operator_")
                        .and_then(|s| s.strip_suffix("_begin"))
                    {
                        // due to the way things work, the end predicate for this is
                        // guarenteed to be in the same level.
                        let end_name = format!("_operator_{}_end", op_name);
                        let pair_id = lit.args[0].as_constant().unwrap();
                        let mut j = i + 1;
                        while !{
                            if let ClauseId::Builtin(ref lit) = children[j].clause {
                                lit.predicate.0 == end_name
                                    && lit.args[0].as_constant() == Some(pair_id)
                            } else {
                                false
                            }
                        } {
                            j += 1;
                        }
                        // at this point j points to the end predicate.

                        let subtree_in_op = &children[i + 1..j];
                        process_operator(
                            subtree_in_op,
                            op_name,
                            lit,
                            rules,
                            res,
                            image_literals,
                            curr_state,
                        );
                        i = j + 1;
                        continue;
                    }
                }
                process_tree(child, rules, res, image_literals, curr_state);
                i += 1;
            }
        }

        process_children(subtree, rules, res, image_literals, &mut curr_state);

        debug_assert!(curr_state.current_merge.is_none());

        if curr_state.current_node.is_some() && tag_with_literal.is_some() {
            let node = curr_state.current_node.unwrap();
            let tagged_node = res.new_node(
                BuildNode::SetLabel {
                    parent: node,
                    label: MODUS_LABEL.to_owned(),
                    value: tag_with_literal.unwrap().to_owned(),
                },
                vec![node],
            );
            curr_state.set_node(tagged_node);
        }
        curr_state.current_node
    }

    for (query, proof) in query_and_proofs.into_iter() {
        debug_assert!(query
            .args
            .iter()
            .all(|x| x.is_constant_or_compound_constant()));
        if let Some(&existing_node_id) = image_literals.get(&query) {
            // TODO: unreachable?
            res.outputs.push(Output {
                node: existing_node_id,
                source_literal: Some(query.clone()),
            });
            continue;
        }
        if let Some(node_id) = process_image(
            &[proof],
            rules,
            &mut res,
            &mut image_literals,
            Some(query.to_string()),
        ) {
            image_literals.insert(query.clone(), node_id);
            res.outputs.push(Output {
                node: node_id,
                source_literal: Some(query.clone()),
            });
        } else {
            panic!("{} does not resolve to any docker instructions.", query);
        }
    }

    res
}

fn join_path(base: &str, path: &str) -> String {
    match Path::new(base).join(path).to_str() {
        Some(s) => s.to_owned(),
        None => panic!("Path containing invalid utf-8 are not allowed."),
    }
}

pub fn plan_from_modusfile(
    mf: Modusfile,
    query: modusfile::Expression,
) -> Result<BuildPlan, Vec<Diagnostic<()>>> {
    // 1. Adds a new clause based on the user's expression query to the Modusfile, `_query :- ...`.
    // 2. Translates the Modusfile to IR.
    // 3. Find proof for `_query`. We need to do this, and not just find proof of the image literal due to any
    //    possible logical constraints.
    // 4. Modify proof to give proof for the single image literal. The other literals, if any, should
    //    only be logic literals.
    //
    // Operators on image literals will not work.

    fn validate_query_expression(query: &modusfile::Expression) -> Result<(), Vec<Diagnostic<()>>> {
        // ensures that no operators were used
        match query {
            modusfile::Expression::Literal(_) => Ok(()),
            modusfile::Expression::OperatorApplication(_, _, _) => {
                Err(vec![Diagnostic::error().with_message(
                    "Operators in queries are currently unsupported.",
                )])
            }
            // There shouldn't be any issue with negation in queries.
            modusfile::Expression::And(_, _, e1, e2) | modusfile::Expression::Or(_, _, e1, e2) => {
                validate_query_expression(e1)?;
                validate_query_expression(e2)
            }
        }
    }

    fn get_image_literal(
        query: &modusfile::Expression,
        mf_with_query: &Modusfile,
        ir_q_clause: &Clause,
    ) -> Result<Literal<IRTerm>, Vec<Diagnostic<()>>> {
        let mut errs = Vec::new();

        if let Err(mut es) = validate_query_expression(query) {
            errs.append(&mut es);
        }

        let query_lits = query.literals();

        let kind_res = mf_with_query.kinds();

        let image_count = query_lits
            .iter()
            .filter(|query_lit| kind_res.pred_kind.get(&query_lit.predicate) == Some(&Kind::Image))
            .count();
        if image_count != 1 {
            errs.push(Diagnostic::error().with_message(format!("There must be exactly one image predicate in the query, but {image_count} were found.")));
        }

        let layer_count = query_lits
            .iter()
            .filter(|query_lit| kind_res.pred_kind.get(&query_lit.predicate) == Some(&Kind::Layer))
            .count();
        if layer_count > 0 {
            errs.push(Diagnostic::error().with_message(format!(
                "Layer predicates in queries are currently unsupported, but we found {layer_count}"
            )));
        }

        if !errs.is_empty() {
            return Err(errs);
        }

        let expression_image_literal = query_lits
            .iter()
            .find(|lit| kind_res.pred_kind.get(&lit.predicate) == Some(&Kind::Image))
            .unwrap();
        let image_literal = ir_q_clause
            .body
            .iter()
            .find(|lit| lit.predicate == expression_image_literal.predicate)
            .expect("should find matching predicate name after translation");
        Ok(image_literal.clone())
    }

    let max_depth = 175;

    let goal_pred = Predicate("_query".to_owned());
    let mut mf_with_query = mf.clone();
    mf_with_query.add_goal(query.clone());
    let ir_clauses: Vec<Clause> = translate_modusfile(&mf_with_query);

    let q_clause = ir_clauses
        .iter()
        .find(|c| c.head.predicate == goal_pred)
        .expect("should find same predicate name after translation");
    let query_goal = &q_clause.body;

    let image_literal = get_image_literal(&query, &mf_with_query, q_clause)?;

    // don't store full tree as this takes a lot of memory, and is probably not needed
    // when building/transpiling
    let success_tree = Result::from(sld::sld(&ir_clauses, &query_goal, max_depth, false))?;
    let proofs = sld::proofs(&success_tree, &ir_clauses, &query_goal);

    let query_and_proofs = proofs
        .into_iter()
        .map(|(_, p)| (image_literal.substitute(&p.valuation), p))
        .collect::<Vec<_>>();
    Ok(build_dag_from_proofs(&query_and_proofs[..], &ir_clauses))
}
