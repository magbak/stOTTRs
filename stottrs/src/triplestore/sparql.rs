pub mod errors;
pub(crate) mod lazy_aggregate;
mod lazy_expressions;
mod lazy_graph_patterns;
mod lazy_order;
mod query_context;
pub mod solution_mapping;
mod sparql_to_polars;

use crate::triplestore::sparql::query_context::Context;
use oxrdf::{NamedNode, Variable};
use std::collections::HashMap;

use super::Triplestore;
use crate::literals::sparql_literal_to_any_value;
use crate::mapping::RDFNodeType;
use crate::triplestore::sparql::errors::SparqlError;
use crate::triplestore::sparql::solution_mapping::SolutionMappings;
use crate::triplestore::sparql::sparql_to_polars::sparql_named_node_to_polars_literal_value;
use polars::frame::DataFrame;
use polars::prelude::{col, IntoLazy};
use polars_core::prelude::{DataType, Series};
use spargebra::term::{NamedNodePattern, TermPattern, TriplePattern};
use spargebra::Query;

pub enum QueryResult {
    Select(DataFrame),
    Construct(Vec<(DataFrame, RDFNodeType)>),
}

impl Triplestore {
    pub fn query(&mut self, query: &str) -> Result<QueryResult, SparqlError> {
        if !self.deduplicated {
            self.deduplicate()
        }
        let query = Query::parse(query, None).map_err(|x| SparqlError::ParseError(x))?;
        self.query_parsed(&query)
        }

    fn query_parsed(&self, query:&Query) -> Result<QueryResult, SparqlError> {
        let context = Context::new();
        match query {
            Query::Select {
                dataset: _,
                pattern,
                base_iri: _,
            } => {
                let SolutionMappings {
                    mappings,
                    columns: _,
                    datatypes: _,
                } = self.lazy_graph_pattern(&pattern, None, &context)?;
                let df = mappings.collect().unwrap();
                let mut cats = vec![];
                for c in df.columns(df.get_column_names()).unwrap() {
                    if let DataType::Categorical(_) = c.dtype() {
                        cats.push(c.name().to_string());
                    }
                }
                let mut lf = df.lazy();
                for c in cats {
                    lf = lf.with_column(col(&c).cast(DataType::Utf8))
                }

                Ok(QueryResult::Select(lf.collect().unwrap()))
            }
            Query::Construct {
                template,
                dataset: _,
                pattern,
                base_iri: _,
            } => {
                let SolutionMappings {
                    mappings,
                    columns: _,
                    datatypes,
                } = self.lazy_graph_pattern(&pattern, None, &context)?;
                let df = mappings.collect().unwrap();
                let mut dfs = vec![];
                for t in template {
                    dfs.push(triple_to_df(&df, &datatypes, t)?);
                }
                Ok(QueryResult::Construct(dfs))
            }
            _ => Err(SparqlError::QueryTypeNotSupported),
        }
    }

    pub fn construct_update(&mut self, query:&str) -> Result<(), SparqlError> {
        let query = Query::parse(query, None).map_err(|x| SparqlError::ParseError(x))?;
        if let Query::Construct { .. } = &query {
            let res = self.query_parsed(&query)?;
            match res {
                QueryResult::Select(_) => {panic!("Should never happen")}
                QueryResult::Construct(dfs) => {
                    for (df,dt) in dfs {
                        self.add_triples(df, dt, None);
                    }
                    Ok(())
                }
            }
        } else {
            Err(SparqlError::QueryTypeNotSupported)
        }
    }
}

fn triple_to_df(
    df: &DataFrame,
    datatypes: &HashMap<Variable, RDFNodeType>,
    t: &TriplePattern,
) -> Result<(DataFrame, RDFNodeType), SparqlError> {
    let len = if triple_has_variable(t) {
        df.height()
    } else {
        1
    };
    let (subj_ser, _) = term_pattern_series(df, datatypes, &t.subject, "subject", len);
    let (verb_ser, _) = named_node_pattern_series(df, datatypes, &t.predicate, "verb", len);
    let (obj_ser, dt) = term_pattern_series(df, datatypes, &t.object, "object", len);
    let df = DataFrame::new(vec![subj_ser, verb_ser, obj_ser]).unwrap();
    Ok((df, dt))
}

fn triple_has_variable(t: &TriplePattern) -> bool {
    if let TermPattern::Variable(_) = t.subject {
        return true;
    }
    if let TermPattern::Variable(_) = t.object {
        return true;
    }
    return false;
}

fn term_pattern_series(
    df: &DataFrame,
    datatypes: &HashMap<Variable, RDFNodeType>,
    tp: &TermPattern,
    name: &str,
    len: usize,
) -> (Series, RDFNodeType) {
    match tp {
        TermPattern::NamedNode(nn) => named_node_series(nn, name, len),
        TermPattern::BlankNode(_) => {
            unimplemented!("Blank node term pattern not supported")
        }
        TermPattern::Literal(lit) => {
            let (anyvalue, dt) = sparql_literal_to_any_value(
                &lit.value().to_string(),
                &Some(lit.datatype().into_owned()),
            );
            let mut any_values = vec![];
            for _ in 0..len {
                any_values.push(anyvalue.clone())
            }
            (
                Series::from_any_values(name, &any_values).unwrap(),
                RDFNodeType::Literal(dt),
            )
        }
        TermPattern::Variable(v) => variable_series(df, datatypes, v, name),
    }
}

fn named_node_pattern_series(
    df: &DataFrame,
    datatypes: &HashMap<Variable, RDFNodeType>,
    nnp: &NamedNodePattern,
    name: &str,
    len: usize,
) -> (Series, RDFNodeType) {
    match nnp {
        NamedNodePattern::NamedNode(nn) => named_node_series(nn, name, len),
        NamedNodePattern::Variable(v) => variable_series(df, datatypes, v, name),
    }
}

fn named_node_series(nn: &NamedNode, name: &str, len: usize) -> (Series, RDFNodeType) {
    let anyvalue = sparql_named_node_to_polars_literal_value(nn)
        .to_anyvalue()
        .unwrap()
        .into_static()
        .unwrap();
    let mut any_values = vec![];
    for _ in 0..len {
        any_values.push(anyvalue.clone())
    }
    (
        Series::from_any_values(name, &any_values).unwrap(),
        RDFNodeType::IRI,
    )
}

fn variable_series(
    df: &DataFrame,
    datatypes: &HashMap<Variable, RDFNodeType>,
    v: &Variable,
    name: &str,
) -> (Series, RDFNodeType) {
    let mut ser = df.column(v.as_str()).unwrap().clone();
    ser.rename(name);
    (ser, datatypes.get(v).unwrap().clone())
}
