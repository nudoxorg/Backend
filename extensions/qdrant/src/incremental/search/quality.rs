//! Exact and approximate ANN quality metadata validation.

use crate::contracts::{ApproximationMetadata, SearchQuality};
use crate::{Error, ModelVersion, Recipe};

pub(super) fn quality_valid(quality: SearchQuality, recipe: Recipe, model: ModelVersion) -> bool {
    match quality {
        SearchQuality::Exact => true,
        SearchQuality::Approximate(ApproximationMetadata {
            recipe: claimed_recipe,
            model: claimed_model,
            recall,
        }) => claimed_recipe == recipe && claimed_model == model && recall.is_valid(),
    }
}

pub(super) fn combine_quality(
    base: SearchQuality,
    page: SearchQuality,
    recipe: Recipe,
    model: ModelVersion,
) -> Result<SearchQuality, Error> {
    if !quality_valid(base, recipe, model) || !quality_valid(page, recipe, model) {
        return Err(Error::ApproximationMismatch);
    }
    match (base, page) {
        (SearchQuality::Exact, SearchQuality::Exact) => Ok(SearchQuality::Exact),
        (SearchQuality::Approximate(metadata), SearchQuality::Exact)
        | (SearchQuality::Exact, SearchQuality::Approximate(metadata)) => {
            Ok(SearchQuality::Approximate(metadata))
        }
        (SearchQuality::Approximate(base), SearchQuality::Approximate(page)) => {
            if base == page {
                Ok(SearchQuality::Approximate(base))
            } else {
                Err(Error::ApproximationMismatch)
            }
        }
    }
}
