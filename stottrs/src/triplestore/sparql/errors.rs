use spargebra::ParseError;
use thiserror::Error;
use crate::mapping::RDFNodeType;

#[derive(Error, Debug)]
pub enum SparqlError {
    #[error("SQL Parsersing Error: {0}")]
    ParseError(ParseError),
    #[error("Query type not supported")]
    QueryTypeNotSupported,
    #[error("Inconsistent datatypes for {}, {:?}, {:?} in context {}", .0, .1, .2, .3)]
    InconsistentDatatypes(String, RDFNodeType, RDFNodeType, String),
    #[error("Variable ?{} not found in context {}",.0, .1)]
    VariableNotFound(String, String)
}
