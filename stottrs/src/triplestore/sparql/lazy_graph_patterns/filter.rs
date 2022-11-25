use super::Triplestore;
use polars::prelude::{col};
use spargebra::algebra::{Expression, GraphPattern};
use log::debug;
use crate::triplestore::sparql::errors::SparqlError;
use crate::triplestore::sparql::query_context::{Context, PathEntry};
use crate::triplestore::sparql::solution_mapping::SolutionMappings;

impl Triplestore {
    pub(crate) fn lazy_filter(
        &self,
        inner: &GraphPattern,
        expression: &Expression,
        input_solution_mappings: Option<SolutionMappings>,
        context: &Context,
    ) -> Result<SolutionMappings, SparqlError> {
        debug!("Processing filter graph pattern");
        let inner_context = context.extension_with(PathEntry::FilterInner);
        let expression_context = context.extension_with(PathEntry::FilterExpression);

        let output_solution_mappings = self.lazy_graph_pattern(
            inner,
            input_solution_mappings,
            &inner_context,
        )?;
        let SolutionMappings { mut mappings, columns, datatypes } = self.lazy_expression(expression, output_solution_mappings, &expression_context)?;
        mappings = mappings
            .filter(col(&expression_context.as_str()))
            .drop_columns([&expression_context.as_str()]);
        Ok(SolutionMappings::new(mappings, columns, datatypes))
    }
}
