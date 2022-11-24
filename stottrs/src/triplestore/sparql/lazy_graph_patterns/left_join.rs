use std::ops::Not;
use super::Triplestore;
use polars::prelude::{col, concat, Expr, IntoLazy, LiteralValue};
use polars_core::series::Series;
use spargebra::algebra::{Expression, GraphPattern};
use log::debug;
use crate::triplestore::sparql::errors::SparqlError;
use crate::triplestore::sparql::query_context::{Context, PathEntry};
use crate::triplestore::sparql::solution_mapping::SolutionMappings;

impl Triplestore {
    pub(crate) fn lazy_left_join(
        &mut self,
        left: &GraphPattern,
        right: &GraphPattern,
        expression: &Option<Expression>,
        solution_mapping: Option<SolutionMappings>,
        context: &Context,
    ) -> Result<SolutionMappings, SparqlError> {
        debug!("Processing left join graph pattern");
        let left_join_distinct_column = context.as_str();
        let left_context = context.extension_with(PathEntry::LeftJoinLeftSide);
        let right_context = context.extension_with(PathEntry::LeftJoinRightSide);
        let expression_context = context.extension_with(PathEntry::LeftJoinExpression);
        let mut left_solution_mappings = self
            .lazy_graph_pattern(
                left,
                solution_mapping,
                &left_context,
            )
            ?;
        let mut left_df = left_solution_mappings
            .mappings
            .with_column(Expr::Literal(LiteralValue::Int64(1)).alias(&left_join_distinct_column))
            .with_column(col(&left_join_distinct_column).cumsum(false).keep_name())
            .collect()
            .expect("Left join collect left problem");

        left_solution_mappings.mappings = left_df.clone().lazy();
        let mut left_columns = left_solution_mappings.columns.clone();
        let mut left_datatypes = left_solution_mappings.datatypes.clone();

        let mut right_solution_mappings = self
            .lazy_graph_pattern(
                right,
                Some(left_solution_mappings),
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
        let SolutionMappings {
            mappings: right_mappings,
            columns: right_columns,
            datatypes: mut right_datatypes,
        } = right_solution_mappings;

        let right_df = right_mappings.collect().expect("Collect right problem");

        left_df = left_df
            .filter(
                &left_df
                    .column(&left_join_distinct_column)
                    .expect("Did not find left helper")
                    .is_in(
                        right_df
                            .column(&left_join_distinct_column)
                            .expect("Did not find right helper"),
                    )
                    .expect("Is in problem")
                    .not(),
            )
            .expect("Filter problem");

        for s in right_df.get_column_names() {
            if !left_df.get_column_names().contains(&s) {
                let left_col =
                    Series::full_null(s, left_df.height(), right_df.column(s).unwrap().dtype());
                left_df.with_column(left_col).unwrap();
            }
        }

        left_df = left_df
            .select(right_df.get_column_names().as_slice())
            .unwrap();
        let mut output_lf =
            concat(vec![left_df.lazy(), right_df.lazy()], true, true).expect("Concat error");
        output_lf = output_lf.drop_columns(&[&left_join_distinct_column]);
        output_lf = output_lf
            .collect()
            .expect("Left join collect problem")
            .lazy();
        for (v, nn) in right_datatypes.drain() {
            left_datatypes.insert(v, nn);
        }
        left_columns.extend(right_columns);

        Ok(SolutionMappings::new(
            output_lf,
            left_columns,
            left_datatypes,
        ))
    }
}