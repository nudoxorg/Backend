use candle_core::{D, DType, Module, Result, Tensor};
pub(crate) use candle_nn::Linear;
use candle_nn::{Init, VarBuilder};

#[derive(Clone)]
pub(crate) struct LayerNorm {
    weight: Tensor,
    bias: Tensor,
    epsilon: f64,
}

pub(crate) fn layer_norm(size: usize, epsilon: f64, variables: VarBuilder) -> Result<LayerNorm> {
    Ok(LayerNorm {
        weight: variables.get_with_hints(size, "weight", Init::Const(1.))?,
        bias: variables.get_with_hints(size, "bias", Init::Const(0.))?,
        epsilon,
    })
}

impl Module for LayerNorm {
    fn forward(&self, input: &Tensor) -> Result<Tensor> {
        let input_dtype = input.dtype();
        let input = input.to_dtype(DType::F32)?;
        let hidden_size = input.dim(D::Minus1)?;
        let mean = (input.sum_keepdim(D::Minus1)? / hidden_size as f64)?;
        let centered = input.broadcast_sub(&mean)?;
        let variance = (centered.sqr()?.sum_keepdim(D::Minus1)? / hidden_size as f64)?;
        let normalized = centered.broadcast_div(&(variance + self.epsilon)?.sqrt()?)?;
        normalized
            .to_dtype(input_dtype)?
            .broadcast_mul(&self.weight)?
            .broadcast_add(&self.bias)
    }
}

pub(crate) fn linear(
    input_size: usize,
    output_size: usize,
    variables: VarBuilder,
) -> Result<Linear> {
    candle_nn::linear(input_size, output_size, variables)
}
