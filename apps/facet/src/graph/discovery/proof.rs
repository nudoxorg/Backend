//! Whether the recipe's canonical key retains enough type information to
//! advertise a transformation. Unknown and erased information never proves
//! nominal type arguments, callable signatures or associated projections.

use crate::graph::model::{Kind, NodeId, World};
use crate::semantics::names::Names;
use crate::semantics::recipes::{self, How, Recipes};
use crate::semantics::types::{TypeExpr, parse, split_top};
use crate::semantics::{bounds, members};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum TypeProof {
    Proven,
    /// A direct generic input: the capability/unification layer must prove it.
    Generic,
    Unknown,
    /// Information needed for identity was discarded by the shape algebra.
    Erased,
}

impl TypeProof {
    pub(super) fn eligible(self) -> bool {
        matches!(self, Self::Proven | Self::Generic)
    }

    fn nested(self) -> Self {
        if self == Self::Generic {
            Self::Erased
        } else {
            self
        }
    }
}

struct Scope<'a> {
    world: &'a World,
    recipes: &'a Recipes,
    names: &'a Names,
    from: NodeId,
    owner: Option<NodeId>,
    generic: HashSet<String>,
}

impl Scope<'_> {
    fn named(&self, path: &[String], args: &[TypeExpr]) -> TypeProof {
        let Some(last) = path.last() else {
            return TypeProof::Unknown;
        };
        if path.first().is_some_and(|name| self.generic.contains(name)) {
            return if path.len() == 1 && args.is_empty() {
                TypeProof::Generic
            } else {
                TypeProof::Unknown
            };
        }
        if last == "Self" {
            return self.owner.map_or(TypeProof::Unknown, |owner| {
                if bounds::generics(
                    &[self.world.node(owner).generics.as_deref().unwrap_or("")],
                    "",
                )
                .is_empty()
                {
                    TypeProof::Proven
                } else {
                    TypeProof::Erased
                }
            });
        }
        let canonical = recipes::canonical_name(last);
        let resolved = self.recipes.resolve(self.world, last, self.from);
        if canonical {
            // A custom `demo::Vec` or `demo::String` cannot inherit the standard
            // constructor's meaning merely because its final name is familiar.
            let qualified_standard = path.len() == 1
                || path
                    .first()
                    .is_some_and(|name| matches!(name.as_str(), "std" | "core" | "alloc"));
            let declared_standard = resolved.is_none_or(|node| {
                matches!(
                    self.world.packages[self.world.node(node).pkg as usize]
                        .name
                        .as_ref(),
                    "std" | "core" | "alloc"
                )
            });
            if !qualified_standard || (path.len() == 1 && !declared_standard) {
                return TypeProof::Erased;
            }
            return recipes::represented_arguments(last).map_or(TypeProof::Proven, |count| {
                if args.len() != count {
                    return TypeProof::Unknown;
                }
                let proof = args
                    .iter()
                    .take(count)
                    .map(|arg| self.expr(arg))
                    .max()
                    .unwrap_or(TypeProof::Unknown);
                // The capability layer currently proves direct generic inputs,
                // not a generic nested inside a structural container.
                proof.nested()
            });
        }
        if !args.is_empty() {
            return TypeProof::Erased;
        }
        let Some(resolved) = resolved else {
            return TypeProof::Unknown;
        };
        let expected = format!("#{resolved}");
        let expr = TypeExpr::Named {
            path: path.to_vec(),
            args: args.to_vec(),
        };
        if self
            .recipes
            .expression_key(self.world, self.from, self.owner, &expr)
            != expected
        {
            return TypeProof::Erased;
        }
        if !bounds::generics(
            &[self.world.node(resolved).generics.as_deref().unwrap_or("")],
            "",
        )
        .is_empty()
        {
            return TypeProof::Erased;
        }
        if path.len() > 1 {
            let Some(declared) = super::capability::absolute_path(self.world, self.from, path)
            else {
                return TypeProof::Unknown;
            };
            let declared = declared.join("::");
            let actual = format!(
                "{}::{}",
                self.world.qual(resolved),
                self.world.node(resolved).name
            );
            let target = self.world.node(resolved);
            let package = self.world.packages[target.pkg as usize]
                .name
                .replace('-', "_");
            let module = self.world.modules[target.module as usize].path.as_ref();
            let full = if module.is_empty() {
                format!("{package}::{}", target.name)
            } else {
                format!("{package}::{module}::{}", target.name)
            };
            return if actual == declared
                || actual.ends_with(&format!("::{declared}"))
                || full == declared
                || full.ends_with(&format!("::{declared}"))
            {
                TypeProof::Proven
            } else {
                TypeProof::Erased
            };
        }
        let candidates = self.names.candidates(last);
        let local: Vec<_> = candidates
            .iter()
            .copied()
            .filter(|&i| self.world.node(i).module == self.world.node(self.from).module)
            .collect();
        let package: Vec<_> = candidates
            .iter()
            .copied()
            .filter(|&i| self.world.node(i).pkg == self.world.node(self.from).pkg)
            .collect();
        let plausible = if !local.is_empty() {
            local.as_slice()
        } else if !package.is_empty() {
            package.as_slice()
        } else {
            candidates
        };
        if plausible == [resolved] {
            TypeProof::Proven
        } else {
            TypeProof::Unknown
        }
    }

    fn output(&self, expr: &TypeExpr) -> TypeProof {
        // The top-level failure argument is intentionally abstract: the
        // producer records `fails`, and the road unwraps the successful value.
        // An explicit applied query cannot claim that omitted error identity.
        if let TypeExpr::Named { path, args } = expr
            && path.last().is_some_and(|name| name == "Result")
            && args.len() == 2
        {
            return self.named(path, &args[..1]);
        }
        self.expr(expr)
    }

    fn expr(&self, expr: &TypeExpr) -> TypeProof {
        match expr {
            TypeExpr::Named { path, args } => self.named(path, args),
            TypeExpr::Ref { inner, .. } | TypeExpr::Binding { ty: inner, .. } => self.expr(inner),
            // An owned value cannot be passed as a raw pointer without an
            // explicit construction. Recipe keys do not preserve that step.
            TypeExpr::Ptr { .. } => TypeProof::Erased,
            TypeExpr::Slice(inner) => self.expr(inner).nested(),
            // The key contains neither an array extent nor a function signature.
            TypeExpr::Array { .. } | TypeExpr::Func { .. } => TypeProof::Erased,
            TypeExpr::Tuple(parts) => parts
                .iter()
                .map(|part| self.expr(part).nested())
                .max()
                .unwrap_or(TypeProof::Proven),
            TypeExpr::Any(_) => TypeProof::Generic,
            TypeExpr::Assoc { .. } | TypeExpr::Infer => TypeProof::Unknown,
            TypeExpr::Never => TypeProof::Proven,
        }
    }
}

pub(super) fn index(world: &World, recipes: &Recipes, names: &Names) -> Vec<TypeProof> {
    recipes
        .table()
        .iter()
        .map(|producer| {
            let node = world.node(producer.node);
            let parent = node.parent.map(|p| world.node(p));
            let lists: Vec<_> = [
                node.generics.as_deref(),
                parent.and_then(|p| p.generics.as_deref()),
            ]
            .into_iter()
            .flatten()
            .collect();
            let scope = Scope {
                world,
                recipes,
                names,
                from: producer.node,
                owner: node.parent,
                generic: bounds::generics(&lists, node.where_.as_deref().unwrap_or(""))
                    .into_iter()
                    .map(|g| g.name)
                    .collect(),
            };
            let owner_generic = node.parent.is_some_and(|owner| {
                !bounds::generics(&[world.node(owner).generics.as_deref().unwrap_or("")], "")
                    .is_empty()
            });
            match producer.how {
                How::Call | How::Method => {
                    if owner_generic || node.quals.iter().any(|qual| qual.as_ref() == "async") {
                        return TypeProof::Erased;
                    }
                    let output = node
                        .ret
                        .as_deref()
                        .map_or(TypeProof::Proven, |ret| scope.output(&parse(ret)));
                    if output != TypeProof::Proven {
                        return output.nested();
                    }
                    members::params(&node.params)
                        .iter()
                        .map(|param| scope.expr(&parse(&param.ty)))
                        .max()
                        .unwrap_or(TypeProof::Proven)
                }
                How::Variant => {
                    if owner_generic {
                        return TypeProof::Erased;
                    }
                    node.ty.as_deref().map_or(TypeProof::Proven, |source| {
                        split_top(source, ',')
                            .iter()
                            .map(|part| {
                                let ty = if node.shape.as_deref() == Some("record") {
                                    part.split_once(':').map_or(part.as_str(), |(_, ty)| ty)
                                } else {
                                    part.as_str()
                                };
                                scope.expr(&parse(ty)).nested()
                            })
                            .max()
                            .unwrap_or(TypeProof::Proven)
                    })
                }
                How::Literal | How::Default => {
                    if !scope.generic.is_empty() {
                        return TypeProof::Erased;
                    }
                    world
                        .kids(producer.node)
                        .iter()
                        .filter(|&&i| world.node(i).kind == Kind::Field)
                        .map(|&i| {
                            world
                                .node(i)
                                .ty
                                .as_deref()
                                .map_or(TypeProof::Unknown, |ty| scope.expr(&parse(ty)).nested())
                        })
                        .max()
                        .unwrap_or(TypeProof::Proven)
                }
            }
        })
        .collect()
}
