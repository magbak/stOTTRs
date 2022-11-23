use super::Mapping;
use crate::ast::{
    Argument, ConstantLiteral, ConstantTerm, Instance, PType, Parameter, Signature, StottrTerm, StottrVariable, Template,
};
use crate::constants::{DEFAULT_PREDICATE_URI_PREFIX, DEFAULT_TEMPLATE_PREFIX, OTTR_TRIPLE};
use crate::mapping::errors::MappingError;
use log::warn;
use oxrdf::vocab::xsd;
use oxrdf::{NamedNode};
use polars::prelude::{col, IntoLazy};
use polars_core::frame::DataFrame;
use polars_core::prelude::DataType;
use uuid::Uuid;
use crate::mapping::ExpandOptions;

impl Mapping {
    pub fn expand_default(
        &mut self,
        mut df: DataFrame,
        pk_col: String,
        fk_cols: Vec<String>,
        template_prefix: Option<String>,
        predicate_prefix_uri: Option<String>,
        options: ExpandOptions,
    ) -> Result<Template, MappingError> {
        let use_template_prefix = template_prefix.unwrap_or(DEFAULT_TEMPLATE_PREFIX.to_string());
        let use_predicate_uri_prefix = predicate_prefix_uri.unwrap_or(DEFAULT_PREDICATE_URI_PREFIX.to_string());
        let mut params = vec![];
        let columns: Vec<String> = df.get_column_names().iter().map(|x| x.to_string()).collect();
        for c in &columns {
            let dt = df.column(&c).unwrap().dtype().clone();

            if c == &pk_col {
                if let DataType::List(..) = dt {
                    todo!()
                }
                if dt != DataType::Utf8 {
                    warn!(
                        "Primary key column {} is not Utf8 but instead {}. Will be cast",
                        &pk_col, dt
                    );
                    df = df
                        .lazy()
                        .with_column(col(&c).cast(DataType::Utf8))
                        .collect()
                        .unwrap();
                }

                params.push(Parameter {
                    optional: false,
                    non_blank: false,
                    ptype: Some(PType::BasicType(xsd::ANY_URI.into_owned(), "xsd:anyURI".to_string())),
                    stottr_variable: StottrVariable {
                        name: c.to_string(),
                    },
                    default_value: None,
                })
            }

            if fk_cols.contains(&c) {
                if let DataType::List(..) = dt {
                    todo!()
                }

                if dt != DataType::Utf8 {
                    warn!(
                        "Foreign key column {} is not Utf8 but instead {}. Will be cast",
                        &c, dt
                    );
                    df = df
                        .lazy()
                        .with_column(col(&c).cast(DataType::Utf8))
                        .collect()
                        .unwrap();
                }

                params.push(Parameter {
                    optional: false,
                    non_blank: false,
                    ptype: Some(PType::BasicType(xsd::ANY_URI.into_owned(), "xsd:anyURI".to_string())),
                    stottr_variable: StottrVariable {
                        name: c.to_string(),
                    },
                    default_value: None,
                })
            } else {
                params.push(Parameter {
                    optional: false,
                    non_blank: false,
                    ptype: None,
                    stottr_variable: StottrVariable {
                        name: c.to_string(),
                    },
                    default_value: None,
                });
            }
        }

        let mut patterns = vec![];
        for c in columns {
            if c != pk_col && !fk_cols.contains(&c) {
                patterns.push(Instance {
                    list_expander: None,
                    template_name: OTTR_TRIPLE.parse().unwrap(),
                    argument_list: vec![
                        Argument {
                            list_expand: false,
                            term: StottrTerm::Variable(StottrVariable {
                                name: pk_col.clone(),
                            }),
                        },
                        Argument {
                            list_expand: false,
                            term: StottrTerm::ConstantTerm(ConstantTerm::Constant(
                                ConstantLiteral::IRI(
                                    NamedNode::new(format!("{}{}", &use_predicate_uri_prefix, c)).unwrap(),
                                ),
                            )),
                        },
                        Argument {
                            list_expand: false,
                            term: StottrTerm::Variable(StottrVariable { name: c.clone() }),
                        },
                    ],
                })
            }
        }

        let template_uuid = Uuid::new_v4().to_string();
        let template_name =format!(
                    "{}{}",use_template_prefix,
                    &template_uuid
                );
        let template = Template {
            signature: Signature {
                template_name: NamedNode::new(template_name.clone()).unwrap(),
                template_prefixed_name: format!("prefix:{}", template_uuid),
                parameter_list: params,
                annotation_list: None,
            },
            pattern_list: patterns,
        };
        self.template_dataset.templates.push(template.clone());
        self.expand(template_name.as_str(), df, options)?;
        Ok(template)
    }
}