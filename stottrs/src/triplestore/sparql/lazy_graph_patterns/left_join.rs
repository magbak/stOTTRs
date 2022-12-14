use super::Triplestore;
use polars::prelude::{col, Expr};
use spargebra::algebra::{Expression, GraphPattern};
use log::debug;
use polars_core::prelude::JoinType;
use crate::triplestore::sparql::errors::SparqlError;
use crate::triplestore::sparql::query_context::{Context, PathEntry};
use crate::triplestore::sparql::solution_mapping::SolutionMappings;

impl Triplestore {
    pub(crate) fn lazy_left_join(
        &self,
        left: &GraphPattern,
        right: &GraphPattern,
        expression: &Option<Expression>,
        solution_mappings: Option<SolutionMappings>,
        context: &Context,
    ) -> Result<SolutionMappings, SparqlError> {
        debug!("Processing left join graph pattern");
        let left_context = context.extension_with(PathEntry::LeftJoinLeftSide);
        let right_context = context.extension_with(PathEntry::LeftJoinRightSide);
        let expression_context = context.extension_with(PathEntry::LeftJoinExpression);
        let mut left_solution_mappings = self
            .lazy_graph_pattern(
                left,
                solution_mappings.clone(),
                &left_context,
            )
            ?;

        let mut right_solution_mappings = self
            .lazy_graph_pattern(
                right,
                Some(left_solution_mappings.clone()),
                &right_context,
            )
            ?;

        if let Some(expr) = expression {
            right_solution_mappings = self
                .lazy_expression(
                    expr,
                    right_solution_mappings,
                    &expression_context,
                )
                ?;
            right_solution_mappings.mappings = right_solution_mappings
                .mappings
                .filter(col(&expression_context.as_str()))
                .drop_columns([&expression_context.as_str()]);
        }
        let SolutionMappings{ mappings: mut right_mappings, columns: mut right_columns, rdf_node_types: mut right_datatypes } = right_solution_mappings;

        let mut join_on:Vec<&String> = left_solution_mappings.columns.intersection(&right_columns).collect();
        join_on.sort();

        let join_on_cols:Vec<Expr> = join_on.iter().map(|x|col(x)).collect();

        if join_on.is_empty() {
            left_solution_mappings.mappings = left_solution_mappings.mappings.join(right_mappings, join_on_cols.as_slice(), join_on_cols.as_slice(), JoinType::Cross)
        } else {
            let all_false = [false].repeat(join_on_cols.len());
            right_mappings = right_mappings.sort_by_exprs(join_on_cols.as_slice(), all_false.as_slice(), false);
            left_solution_mappings.mappings = left_solution_mappings.mappings.sort_by_exprs(
                join_on_cols.as_slice(),
                all_false.as_slice(),
                false,
            );
            left_solution_mappings.mappings = left_solution_mappings.mappings.join(right_mappings, join_on_cols.as_slice(), join_on_cols.as_slice(), JoinType::Left)
        }
        for c in right_columns.drain() {
           left_solution_mappings.columns.insert(c);
        }
        for (var, dt) in right_datatypes.drain() {
            if let Some(dt_left) = left_solution_mappings.rdf_node_types.get(&var) {
                //TODO: handle compatibility
                // if &dt != dt_left {
                //     return Err(SparqlError::InconsistentDatatypes(var.clone(), dt_left.clone(), dt, context.clone()))
                // }
            } else {
                left_solution_mappings.rdf_node_types.insert(var, dt);
            }
        }

        Ok(left_solution_mappings)
    }
}
