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

mod builtin;
mod dockerfile;
mod logic;
mod modusfile;
mod sld;
mod transpiler;
mod unification;
mod wellformed;

extern crate lazy_static;

#[macro_use]
extern crate fp_core;

use clap::{crate_version, App, Arg};
use colored::Colorize;
use std::fs;

use dockerfile::ResolvedDockerfile;
use modusfile::Modusfile;

use crate::transpiler::prove_goal;

fn main() {
    let matches = App::new("modus")
        .version(crate_version!())
        .about("Datalog-based container build system")
        .subcommand(
            App::new("transpile")
                .arg(
                    Arg::with_name("FILE")
                        .required(true)
                        .help("Sets the input Modusfile")
                        .index(1),
                )
                .arg(
                    Arg::with_name("QUERY")
                        .required(true)
                        .help("Specifies the build target(s)")
                        .index(2),
                )
                .arg(
                    Arg::with_name("proof")
                        .short("p")
                        .long("proof")
                        .help("Prints the proof tree of the target image"),
                )
                .arg(
                    Arg::with_name("output")
                        .short("o")
                        .value_name("FILE")
                        .long("output")
                        .help("Sets the output Dockerfile")
                        .takes_value(true),
                ),
        )
        .subcommand(
            App::new("proof")
                .arg(
                    Arg::with_name("FILE")
                        .required(true)
                        .help("Sets the input Modusfile")
                        .index(1),
                )
                .arg(
                    Arg::with_name("QUERY")
                        .help("Specifies the target to prove")
                        .index(2),
                ),
        )
        .get_matches();

    match matches.subcommand() {
        ("transpile", Some(sub)) => {
            let input_file = sub.value_of("FILE").unwrap();
            let query: logic::Literal = sub.value_of("QUERY").map(|s| s.parse().unwrap()).unwrap();

            let file_content = fs::read_to_string(input_file).unwrap();
            let mf: Modusfile = file_content.parse().unwrap();

            let df: ResolvedDockerfile = transpiler::transpile(mf, query);

            println!("{}", df);
        }
        ("proof", Some(sub)) => {
            let input_file = sub.value_of("FILE").unwrap();
            let query: Option<logic::Literal> = sub.value_of("QUERY").map(|l| l.parse().unwrap());

            let file_content = fs::read_to_string(input_file).unwrap();
            match (file_content.parse::<Modusfile>(), query) {
                (Ok(modus_f), None) => println!(
                    "Parsed {} successfully. Found {} clauses.",
                    input_file,
                    modus_f.0.len()
                ),
                (Ok(modus_f), Some(l)) => match prove_goal(&modus_f, &vec![l.clone()]) {
                    Ok(proofs) => {
                        println!(
                            "{} proof(s) found for query {}",
                            proofs.len(),
                            l.to_string().blue()
                        );
                        // TODO: pretty print proof, we could use the 'colored' library for terminal colors
                    }
                    Err(e) => println!("{}", e),
                },
                (Err(error), _) => {
                    println!("Didn't parse {} successfully, error: {}", input_file, error)
                }
            }
        }
        _ => (),
    }
}
