use super::context::{ParseContext, ParseState};
use super::{error::Parse, Result};
use ir::generics::{Constraint, ConstExpr, GenericArg, Generics, Term, TraitRef, TypeExpr, Variance};
use ir::parameter::{ConstParam, LifetimeParam, Parameter, TypeParam, TypeParamOrigin};
use ir::protocols::GenericBound;
use ir::ty::Type;

impl ParseContext {
	pub(super) fn generic_params(&self, generics: &rustdoc_types::Generics) -> Option<Generics> {
		if generics.params.is_empty() && generics.where_predicates.is_empty() {
			return None;
		}

		let mut params = Vec::new();

		for param in &generics.params {
			match &param.kind {
				rustdoc_types::GenericParamDefKind::Type { bounds: _, default, is_synthetic } => {
					if !is_synthetic {
						params.push(Parameter::Type(TypeParam {
							name:         Some(param.name.clone()),
							kind:         ir::generics::Kind::Type,
							variance:     Variance::Invariant,
							default_type: default
								.as_ref()
								.and_then(|ty| self.type_(&ty).ok())
								.map(|ty| TypeExpr { name: format!("{:?}", ty), args: vec![] }),
							params:       None,
							origin:       TypeParamOrigin::Free,
						}));
					}
				}
				rustdoc_types::GenericParamDefKind::Const { type_, default } => {
					if let Ok(ty) = self.type_(type_) {
						params.push(Parameter::Const(ConstParam {
							name:          param.name.clone(),
							r#type:        TypeExpr { name: format!("{:?}", ty), args: vec![] },
							default_value: default.as_ref().map(|d| ConstExpr::Var(d.clone())),
						}));
					}
				}
				rustdoc_types::GenericParamDefKind::Lifetime { outlives: _ } => {
					params.push(Parameter::Lifetime(LifetimeParam {
						name:     param.name.clone(),
						variance: Variance::Invariant,
					}));
				}
			}
		}

		let constraints = self.where_predicates(&generics.where_predicates).unwrap_or_default();

		Some(Generics { params, constraints })
	}

	pub(super) fn where_predicates(
		&self,
		predicates: &[rustdoc_types::WherePredicate],
	) -> Result<Vec<Constraint>> {
		predicates
			.iter()
			.map(|pred| match pred {
				rustdoc_types::WherePredicate::BoundPredicate { type_, bounds, generic_params: _ } => {
					let param_name = match type_ {
						rustdoc_types::Type::Generic(name) => name.clone(),
						_ => format!("{:?}", type_),
					};

					bounds
						.iter()
						.map(|bound| self.generic_bound_to_constraint(&param_name, bound))
						.collect::<Result<Vec<_>>>()
				}
				rustdoc_types::WherePredicate::EqPredicate { lhs, rhs } => {
					let lhs_str = format!("{:?}", lhs);
					Ok(vec![Constraint::AssociatedTypeBound {
						param:      lhs_str.clone(),
						assoc_name: lhs_str,
						bound:      TypeExpr { name: format!("{:?}", rhs), args: vec![] },
					}])
				}
				rustdoc_types::WherePredicate::LifetimePredicate { lifetime, outlives } => Ok(
					outlives
						.iter()
						.map(|o| Constraint::LifetimeBound { shorter: lifetime.clone(), longer: o.clone() })
						.collect(),
				),
			})
			.collect::<Result<Vec<Vec<_>>>>()
			.map(|v| v.into_iter().flatten().collect())
	}

	pub(super) fn generic_bound_to_constraint(
		&self,
		param: &str,
		bound: &rustdoc_types::GenericBound,
	) -> Result<Constraint> {
		match bound {
			rustdoc_types::GenericBound::TraitBound { trait_, generic_params: _, modifier: _ } => {
				let trait_ref = self.path_to_trait_ref(trait_)?;
				Ok(Constraint::TraitBound { param: param.to_string(), trait_ref })
			}
			rustdoc_types::GenericBound::Use(_) => Ok(Constraint::TraitBound {
				param:     param.to_string(),
				trait_ref: TraitRef { name: "Use".to_string(), args: vec![] },
			}),
			rustdoc_types::GenericBound::Outlives(lifetime) => {
				Ok(Constraint::LifetimeBound { shorter: param.to_string(), longer: lifetime.clone() })
			}
		}
	}

	pub(super) fn trait_bounds(&self, bounds: &[rustdoc_types::GenericBound]) -> Result<Vec<TraitRef>> {
		bounds
			.iter()
			.filter_map(|bound| match bound {
				rustdoc_types::GenericBound::TraitBound { trait_, .. } => {
					Some(self.path_to_trait_ref(trait_))
				}
				_ => None,
			})
			.collect()
	}

	pub(super) fn generic_bounds(
		&self,
		bounds: &[rustdoc_types::GenericBound],
	) -> Result<Vec<GenericBound>> {
		bounds
			.iter()
			.map(|bound| match bound {
				rustdoc_types::GenericBound::TraitBound { trait_, .. } => {
					let trait_ref = self.path_to_trait_ref(trait_)?;
					Ok(GenericBound::Trait(trait_ref))
				}
				rustdoc_types::GenericBound::Outlives(lifetime) => {
					Ok(GenericBound::Lifetime(lifetime.clone()))
				}
				rustdoc_types::GenericBound::Use(_) => {
					Ok(GenericBound::Trait(TraitRef { name: "Use".into(), args: vec![] }))
				}
			})
			.collect()
	}

	pub(super) fn generic_args(&self, args: &rustdoc_types::GenericArgs) -> Result<Vec<GenericArg>> {
		match args {
			rustdoc_types::GenericArgs::AngleBracketed { args, constraints } => {
				let mut result = Vec::new();

				for constraint in constraints {
					let args = constraint.args.as_ref().map(|a| self.generic_args(a)).transpose()?;
					let term = match &constraint.binding {
						rustdoc_types::AssocItemConstraintKind::Equality(term) => {
							self.map_rustdoc_term(term.clone())
						}
						rustdoc_types::AssocItemConstraintKind::Constraint(bounds) => {
							let inner = bounds
								.iter()
								.map(|t| {
									let parsed = self.generic_bounds(std::slice::from_ref(t))?;
									Ok::<_, super::error::Parse>(parsed.into_iter().map(|b| match b {
										GenericBound::Trait(tr) => {
											Constraint::TraitBound { param: String::new(), trait_ref: tr }
										}
										GenericBound::Lifetime(lt) => {
											Constraint::LifetimeBound { shorter: String::new(), longer: lt }
										}
									}))
								})
								.collect::<Result<Vec<_>>>()?
								.into_iter()
								.flatten()
								.collect();
							Term::Bound(inner)
						}
					};
					result.push(GenericArg::Constraint(Constraint::AssociatedItem {
						name: constraint.name.clone(),
						args,
						term,
					}));
				}

				for arg in args {
					match arg {
						rustdoc_types::GenericArg::Lifetime(lt) => {
							result.push(GenericArg::Lifetime(lt.clone()));
						}
						rustdoc_types::GenericArg::Type(ty) => {
let parsed_ty = self.type_(&ty)?;
							result.push(GenericArg::Type(parsed_ty));
						}
						rustdoc_types::GenericArg::Const(c) => {
							result.push(GenericArg::ConstExpr(ConstExpr::Var(c.expr.clone())));
						}
						rustdoc_types::GenericArg::Infer => {
							result.push(GenericArg::Type(Type::Infer));
						}
					}
				}

				Ok(result)
			}
			rustdoc_types::GenericArgs::Parenthesized { inputs, output } => {
				let mut result = Vec::new();

				for input in inputs {
					let parsed_ty = self.type_(input)?;
					result.push(GenericArg::Type(parsed_ty));
				}

				if let Some(output) = output {
					let parsed_output = self.type_(output)?;
					result.push(GenericArg::Type(parsed_output));
				}

				Ok(result)
			}
			rustdoc_types::GenericArgs::ReturnTypeNotation => Ok(vec![]),
		}
	}
}
