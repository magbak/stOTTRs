mod constant_terms;
pub mod default;
pub mod errors;
mod validation_inference;

use crate::ast::{
    ConstantLiteral, ConstantTerm, Instance, ListExpanderType, PType, Signature, StottrTerm,
    Template,
};
use crate::constants::OTTR_TRIPLE;
use crate::document::document_from_str;
use crate::errors::MapperError;
use crate::io_funcs::create_folder_if_not_exists;
use crate::mapping::constant_terms::constant_to_expr;
use crate::mapping::errors::MappingError;
use crate::templates::TemplateDataset;
use crate::triplestore::{TripleType, TriplesToAdd, Triplestore};
use log::debug;
use oxrdf::vocab::xsd;
use oxrdf::{NamedNode, NamedNodeRef, Triple};
use polars::lazy::prelude::{col, Expr};
use polars::prelude::{DataFrame, IntoLazy, PolarsError};
use polars_core::series::Series;
use rayon::iter::ParallelDrainRange;
use rayon::iter::ParallelIterator;
use std::cmp::min;
use std::collections::HashMap;
use std::error::Error;
use std::io::Write;
use std::path::Path;
use std::time::Instant;
use uuid::Uuid;

pub struct Mapping {
    template_dataset: TemplateDataset,
    pub triplestore: Triplestore,
}

pub struct ExpandOptions {
    pub language_tags: Option<HashMap<String, String>>,
    pub unique_subsets: Option<Vec<Vec<String>>>,
}

struct OTTRTripleInstance {
    df: DataFrame,
    dynamic_columns: HashMap<String, PrimitiveColumn>,
    static_columns: HashMap<String, StaticColumn>,
    has_unique_subset: bool,
}

#[derive(Clone)]
struct StaticColumn {
    constant_term: ConstantTerm,
    ptype: Option<PType>,
}

impl Default for ExpandOptions {
    fn default() -> Self {
        ExpandOptions {
            language_tags: None,
            unique_subsets: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PrimitiveColumn {
    pub rdf_node_type: RDFNodeType,
    pub language_tag: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RDFNodeType {
    IRI,
    BlankNode,
    Literal(NamedNode),
    None,
}

impl RDFNodeType {
    pub fn is_lit_type(&self, nnref: NamedNodeRef) -> bool {
        if let RDFNodeType::Literal(l) = self {
            if l.as_ref() == nnref {
                return true;
            }
        }
        false
    }

    pub fn is_bool(&self) -> bool {
        self.is_lit_type(xsd::BOOLEAN)
    }

    pub fn is_float(&self) -> bool {
        self.is_lit_type(xsd::FLOAT)
    }

    pub(crate) fn find_triple_type(&self) -> TripleType {
        let triple_type = if let RDFNodeType::IRI = self {
            TripleType::ObjectProperty
        } else if let RDFNodeType::Literal(lit) = self {
            if lit.as_ref() == xsd::STRING {
                TripleType::StringProperty
            } else {
                TripleType::NonStringProperty
            }
        } else {
            todo!("Triple type {:?} not supported", self)
        };
        triple_type
    }
}

#[derive(Debug, PartialEq)]
pub struct MappingReport {}

impl Mapping {
    pub fn new(template_dataset: &TemplateDataset, caching_folder: Option<String>) -> Mapping {
        match env_logger::try_init() {
            _ => {}
        }
        Mapping {
            template_dataset: template_dataset.clone(),
            triplestore: Triplestore::new(caching_folder),
        }
    }

    pub fn from_folder<P: AsRef<Path>>(
        path: P,
        caching_folder: Option<String>,
    ) -> Result<Mapping, Box<dyn Error>> {
        let dataset = TemplateDataset::from_folder(path)?;
        Ok(Mapping::new(&dataset, caching_folder))
    }

    pub fn from_file<P: AsRef<Path>>(
        path: P,
        caching_folder: Option<String>,
    ) -> Result<Mapping, Box<dyn Error>> {
        let dataset = TemplateDataset::from_file(path)?;
        Ok(Mapping::new(&dataset, caching_folder))
    }

    pub fn from_str(s: &str, caching_folder: Option<String>) -> Result<Mapping, Box<dyn Error>> {
        let doc = document_from_str(s.into())?;
        let dataset = TemplateDataset::new(vec![doc])?;
        Ok(Mapping::new(&dataset, caching_folder))
    }

    pub fn from_strs(
        ss: Vec<&str>,
        caching_folder: Option<String>,
    ) -> Result<Mapping, Box<dyn Error>> {
        let mut docs = vec![];
        for s in ss {
            let doc = document_from_str(s.into())?;
            docs.push(doc);
        }
        let dataset = TemplateDataset::new(docs)?;
        Ok(Mapping::new(&dataset, caching_folder))
    }

    pub fn write_n_triples(&mut self, buffer: &mut dyn Write) -> Result<(), PolarsError> {
        self.triplestore
            .write_n_triples_all_dfs(buffer, 1024)
            .unwrap();
        Ok(())
    }

    pub fn write_native_parquet(&mut self, path: &str) -> Result<(), MapperError> {
        self.triplestore
            .write_native_parquet(Path::new(path))
            .map_err(|x| MapperError::MappingError(x))
    }

    pub fn export_oxrdf_triples(&mut self) -> Result<Vec<Triple>, MappingError> {
        self.triplestore.export_oxrdf_triples()
    }

    fn resolve_template(&self, s: &str) -> Result<&Template, MappingError> {
        if let Some(t) = self.template_dataset.get(s) {
            return Ok(t);
        } else {
            let mut split_colon = s.split(":");
            let prefix_maybe = split_colon.next();
            if let Some(prefix) = prefix_maybe {
                if let Some(nn) = self.template_dataset.prefix_map.get(prefix) {
                    let possible_template_name = nn.as_str().to_string()
                        + split_colon.collect::<Vec<&str>>().join(":").as_str();
                    if let Some(t) = self.template_dataset.get(&possible_template_name) {
                        return Ok(t);
                    } else {
                        return Err(MappingError::NoTemplateForTemplateNameFromPrefix(
                            possible_template_name,
                        ));
                    }
                }
            }
        }
        Err(MappingError::TemplateNotFound(s.to_string()))
    }

    pub fn expand(
        &mut self,
        template: &str,
        df: DataFrame,
        options: ExpandOptions,
    ) -> Result<MappingReport, MappingError> {
        let now = Instant::now();
        let target_template = self.resolve_template(template)?.clone();
        let target_template_name = target_template.signature.template_name.as_str().to_string();
        let columns =
            self.validate_infer_dataframe_columns(&target_template.signature, &df, &options)?;
        let ExpandOptions {
            language_tags: _,
            unique_subsets: unique_subsets_opt,
        } = options;
        let unique_subsets = if let Some(unique_subsets) = unique_subsets_opt {
            unique_subsets
        } else {
            vec![]
        };
        let call_uuid = Uuid::new_v4().to_string();

        if let Some(caching_folder) = &self.triplestore.caching_folder {
            create_folder_if_not_exists(Path::new(&caching_folder))?;
            let n_50_mb = (df.estimated_size() / 50_000_000) + 1;
            let chunk_size = df.height() / n_50_mb;
            let mut offset = 0i64;
            loop {
                let to_row = min(df.height(), offset as usize + chunk_size);
                let df_slice = df.slice_par(offset, to_row);
                offset += chunk_size as i64;
                let result_vec = self._expand(
                    &target_template_name,
                    df_slice,
                    columns.clone(),
                    HashMap::new(),
                    unique_subsets.clone(),
                )?;
                self.process_results(result_vec, &call_uuid)?;
                debug!("Finished processing {} rows", to_row);
                if offset >= df.height() as i64 {
                    break;
                }
            }
        } else {
            let result_vec = self._expand(
                &target_template_name,
                df,
                columns,
                HashMap::new(),
                unique_subsets,
            )?;
            self.process_results(result_vec, &call_uuid)?;
            debug!("Expansion took {} seconds", now.elapsed().as_secs_f32());
        }
        Ok(MappingReport {})
    }

    fn _expand(
        &self,
        name: &str,
        mut df: DataFrame,
        dynamic_columns: HashMap<String, PrimitiveColumn>,
        static_columns: HashMap<String, StaticColumn>,
        unique_subsets: Vec<Vec<String>>,
    ) -> Result<Vec<OTTRTripleInstance>, MappingError> {
        //At this point, the lf should have columns with names appropriate for the template to be instantiated (named_node).
        if let Some(template) = self.template_dataset.get(name) {
            if template.signature.template_name.as_str() == OTTR_TRIPLE {
                Ok(vec![OTTRTripleInstance {
                    df,
                    dynamic_columns,
                    static_columns,
                    has_unique_subset: !unique_subsets.is_empty(),
                }])
            } else {
                let mut series_map: HashMap<String, Series> = df
                    .get_columns_mut()
                    .drain(..)
                    .map(|x| (x.name().to_string(), x))
                    .collect();
                let number_of_series_map =
                    get_number_per_series_map(&template.pattern_list, &dynamic_columns);
                let mut series_keys: Vec<&String> = number_of_series_map.keys().collect();
                series_keys.sort();

                let now = Instant::now();
                let mut repeated_series_names = vec![];
                for k in &series_keys {
                    repeated_series_names
                        .extend([*k].repeat(*number_of_series_map.get(*k).unwrap() as usize))
                }
                let repeated_series_clones: Vec<(&String, Series)> = repeated_series_names
                    .par_drain(..)
                    .map(|x| (x, series_map.get(x).unwrap().clone()))
                    .collect();
                let mut cloned_series_map: HashMap<&String, Vec<Series>> =
                    HashMap::from_iter(series_keys.into_iter().map(|x| (x, vec![])));
                for (k, ser) in repeated_series_clones {
                    cloned_series_map.get_mut(&k).unwrap().push(ser);
                }
                let mut expand_params_vec = vec![];
                for i in &template.pattern_list {
                    let mut instance_series = vec![];
                    let vs = get_variable_names(i);
                    for v in vs {
                        let mut found = false;
                        if let Some(series_vec) = cloned_series_map.get_mut(v) {
                            if let Some(series) = series_vec.pop() {
                                instance_series.push(series);
                                found = true;
                            }
                        }
                        if !found {
                            instance_series.push(series_map.remove(v).unwrap());
                        }
                    }
                    expand_params_vec.push((i, DataFrame::new(instance_series).unwrap()));
                }

                debug!("Cloning args took {} seconds", now.elapsed().as_secs_f64());

                let results: Vec<Result<Vec<OTTRTripleInstance>, MappingError>> = expand_params_vec
                    .par_drain(..)
                    .map(|(i, df)| {
                        let target_template =
                            self.template_dataset.get(i.template_name.as_str()).unwrap();
                        let (
                            instance_df,
                            instance_dynamic_columns,
                            instance_static_columns,
                            new_unique_subsets,
                        ) = create_remapped(
                            i,
                            &target_template.signature,
                            df,
                            &dynamic_columns,
                            &static_columns,
                            &unique_subsets,
                        )?;

                        self._expand(
                            i.template_name.as_str(),
                            instance_df,
                            instance_dynamic_columns,
                            instance_static_columns,
                            new_unique_subsets,
                        )
                    })
                    .collect();
                let mut results_ok = vec![];
                for r in results {
                    results_ok.push(r?)
                }

                Ok(flatten(results_ok))
            }
        } else {
            Err(MappingError::TemplateNotFound(name.to_string()))
        }
    }

    fn process_results(
        &mut self,
        mut result_vec: Vec<OTTRTripleInstance>,
        call_uuid: &String,
    ) -> Result<(), MappingError> {
        let now = Instant::now();
        let triples: Vec<
            Result<(DataFrame, RDFNodeType, Option<String>, Option<String>, bool), MappingError>,
        > = result_vec
            .par_drain(..)
            .map(|i| create_triples(i))
            .collect();
        let mut ok_triples = vec![];
        for t in triples {
            ok_triples.push(t?);
        }
        let mut all_triples_to_add = vec![];
        for (df, rdf_node_type, language_tag, verb, has_unique_subset) in ok_triples {
            all_triples_to_add.push(TriplesToAdd {
                df,
                object_type: rdf_node_type,
                language_tag,
                static_verb_column: verb,
                has_unique_subset,
            });
        }
        self.triplestore
            .add_triples_vec(all_triples_to_add, call_uuid)?;

        debug!(
            "Result processing took {} seconds",
            now.elapsed().as_secs_f32()
        );
        Ok(())
    }
}

fn get_number_per_series_map(
    instances: &Vec<Instance>,
    dynamic_columns: &HashMap<String, PrimitiveColumn>,
) -> HashMap<String, u16> {
    let mut out_map: HashMap<String, u16> =
        dynamic_columns.keys().map(|k| (k.clone(), 0)).collect();
    for i in instances {
        for v in get_variable_names(i) {
            *out_map.get_mut(v).unwrap() += 1;
        }
    }
    out_map
}

fn get_variable_names(i: &Instance) -> Vec<&String> {
    let mut out_vars = vec![];
    for a in &i.argument_list {
        if let StottrTerm::Variable(v) = &a.term {
            out_vars.push(&v.name);
        } else if let StottrTerm::List(..) = &a.term {
            todo!();
        }
    }
    out_vars
}

fn create_triples(
    i: OTTRTripleInstance,
) -> Result<(DataFrame, RDFNodeType, Option<String>, Option<String>, bool), MappingError> {
    let OTTRTripleInstance {
        df,
        mut dynamic_columns,
        static_columns,
        has_unique_subset,
    } = i;

    let mut expressions = vec![];

    let mut verb = None;
    for (k, sc) in static_columns {
        if k == "verb" {
            if let ConstantTerm::Constant(c) = &sc.constant_term {
                if let ConstantLiteral::IRI(nn) = c {
                    verb = Some(nn.as_str().to_string());
                } else {
                    return Err(MappingError::InvalidPredicateConstant(
                        sc.constant_term.clone(),
                    ));
                }
            } else {
                return Err(MappingError::InvalidPredicateConstant(
                    sc.constant_term.clone(),
                ));
            }
        } else {
            let (expr, mapped_column) =
                create_dynamic_expression_from_static(&k, &sc.constant_term, &sc.ptype)?;
            expressions.push(expr.alias(&k));
            dynamic_columns.insert(k, mapped_column);
        }
    }
    let mut lf = df.lazy();
    for e in expressions {
        lf = lf.with_column(e);
    }

    let mut keep_cols = vec![col("subject"), col("object")];
    if verb.is_none() {
        keep_cols.push(col("verb"));
    }
    lf = lf.select(keep_cols.as_slice());
    let df = lf.collect().expect("Collect problem");
    let PrimitiveColumn {
        rdf_node_type,
        language_tag,
    } = dynamic_columns.remove("object").unwrap();
    Ok((df, rdf_node_type, language_tag, verb, has_unique_subset))
}

fn create_dynamic_expression_from_static(
    column_name: &str,
    constant_term: &ConstantTerm,
    ptype: &Option<PType>,
) -> Result<(Expr, PrimitiveColumn), MappingError> {
    let (mut expr, _, rdf_node_type, language_tag) = constant_to_expr(constant_term, ptype)?;
    let mapped_column = PrimitiveColumn {
        rdf_node_type,
        language_tag,
    };
    expr = expr.alias(column_name);
    Ok((expr, mapped_column))
}

fn create_remapped(
    instance: &Instance,
    signature: &Signature,
    df: DataFrame,
    dynamic_columns: &HashMap<String, PrimitiveColumn>,
    constant_columns: &HashMap<String, StaticColumn>,
    unique_subsets: &Vec<Vec<String>>,
) -> Result<
    (
        DataFrame,
        HashMap<String, PrimitiveColumn>,
        HashMap<String, StaticColumn>,
        Vec<Vec<String>>,
    ),
    MappingError,
> {
    let now = Instant::now();
    let mut new_dynamic_columns = HashMap::new();
    let mut new_constant_columns = HashMap::new();
    let mut existing = vec![];
    let mut new = vec![];
    let mut new_dynamic_from_constant = vec![];
    let mut to_expand = vec![];
    let mut expressions = vec![];
    for (original, target) in instance
        .argument_list
        .iter()
        .zip(signature.parameter_list.iter())
    {
        let target_colname = &target.stottr_variable.name;
        if original.list_expand {
            to_expand.push(target_colname.clone());
        }
        match &original.term {
            StottrTerm::Variable(v) => {
                if let Some(c) = dynamic_columns.get(&v.name) {
                    existing.push(&v.name);
                    new.push(target_colname);
                    new_dynamic_columns.insert(target_colname.clone(), c.clone());
                } else if let Some(c) = constant_columns.get(&v.name) {
                    new_constant_columns.insert(target_colname.clone(), c.clone());
                } else {
                    return Err(MappingError::UnknownVariableError(v.name.clone()));
                }
            }
            StottrTerm::ConstantTerm(ct) => {
                if original.list_expand {
                    let (expr, primitive_column) =
                        create_dynamic_expression_from_static(target_colname, ct, &target.ptype)?;
                    expressions.push(expr);
                    new_dynamic_columns.insert(target_colname.clone(), primitive_column);
                    new_dynamic_from_constant.push(target_colname);
                } else {
                    let static_column = StaticColumn {
                        constant_term: ct.clone(),
                        ptype: target.ptype.clone(),
                    };
                    new_constant_columns.insert(target_colname.clone(), static_column);
                }
            }
            StottrTerm::List(_) => {
                todo!()
            }
        }
    }
    let mut lf = df.lazy();

    // TODO: Remove workaround likely bug in Pola.rs 0.25.1
    lf = lf
        .rename(existing.as_slice(), new.as_slice())
        .collect()
        .unwrap()
        .lazy();

    for expr in expressions {
        lf = lf.with_column(expr);
    }
    let new_column_expressions: Vec<Expr> = new
        .iter()
        .chain(new_dynamic_from_constant.iter())
        .map(|x| col(x))
        .collect();
    lf = lf.select(new_column_expressions.as_slice());

    let mut new_unique_subsets = vec![];
    if let Some(le) = &instance.list_expander {
        let to_expand_cols: Vec<Expr> = to_expand.iter().map(|x| col(x)).collect();
        match le {
            ListExpanderType::Cross => {
                for c in to_expand_cols {
                    lf = lf.explode(vec![c]);
                }
            }
            ListExpanderType::ZipMin => {
                lf = lf.explode(to_expand_cols.clone());
                lf = lf.drop_nulls(Some(to_expand_cols));
            }
            ListExpanderType::ZipMax => {
                lf = lf.explode(to_expand_cols);
            }
        }
        //Todo: List expanders for constant terms..
    } else {
        for unique_subset in unique_subsets {
            if unique_subset.iter().all(|x| existing.contains(&x)) {
                let mut new_subset = vec![];
                for x in unique_subset.iter() {
                    new_subset.push(
                        new.get(existing.iter().position(|e| e == &x).unwrap())
                            .unwrap()
                            .to_string(),
                    );
                }
                new_unique_subsets.push(new_subset);
            }
        }
    }
    let df = lf.collect().unwrap();
    debug!(
        "Creating remapped took {} seconds",
        now.elapsed().as_secs_f32()
    );
    Ok((
        df,
        new_dynamic_columns,
        new_constant_columns,
        new_unique_subsets,
    ))
}

//From: https://users.rust-lang.org/t/flatten-a-vec-vec-t-to-a-vec-t/24526/3
fn flatten<T>(nested: Vec<Vec<T>>) -> Vec<T> {
    nested.into_iter().flatten().collect()
}
