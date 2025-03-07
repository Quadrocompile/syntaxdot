// Copyright 2020 The SqueezeBert authors and The HuggingFace Inc. team.
// Copyright (c) 2020 TensorDot.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::borrow::Borrow;

use syntaxdot_tch_ext::PathExt;
use tch::Tensor;

use crate::error::TransformerError;
use crate::models::layer_output::LayerOutput;
use crate::models::squeeze_bert::{SqueezeBertConfig, SqueezeBertLayer};
use crate::models::Encoder;
use crate::util::LogitsMask;

/// SqueezeBERT encoder.
///
/// Even though SqueezeBERT uses *[batch_size, hidden_size, seq_len]*
/// format internally, the encoder accepts the regular *[batch_size,
/// seq_len, hidden_size]* format.
#[derive(Debug)]
pub struct SqueezeBertEncoder {
    layers: Vec<SqueezeBertLayer>,
}

impl SqueezeBertEncoder {
    pub fn new<'a>(
        vs: impl Borrow<PathExt<'a>>,
        config: &SqueezeBertConfig,
    ) -> Result<Self, TransformerError> {
        let vs = vs.borrow();

        let layers = (0..config.num_hidden_layers)
            .map(|layer| SqueezeBertLayer::new(vs / format!("layer_{}", layer), config))
            .collect::<Result<_, _>>()?;

        Ok(SqueezeBertEncoder { layers })
    }
}

impl Encoder for SqueezeBertEncoder {
    fn encode(
        &self,
        input: &Tensor,
        attention_mask: Option<&Tensor>,
        train: bool,
    ) -> Result<Vec<LayerOutput>, TransformerError> {
        let attention_mask = attention_mask.map(LogitsMask::from_bool_mask).transpose()?;

        // [batch_size, seq_len, hidden_size] -> [batch_size, hidden_size, seq_len]
        let mut hidden_states = input.f_permute(&[0, 2, 1])?;

        let mut all_layer_outputs = Vec::with_capacity(self.layers.len() + 1);
        all_layer_outputs.push(LayerOutput::Embedding(hidden_states.shallow_clone()));

        for layer in &self.layers {
            let layer_output = layer.forward_t(&hidden_states, attention_mask.as_ref(), train)?;

            hidden_states = layer_output.output().shallow_clone();
            all_layer_outputs.push(layer_output);
        }

        // Convert hidden states to [batch_size, seq_len, hidden_size].
        for layer_output in &mut all_layer_outputs {
            *layer_output.output_mut() = layer_output.output().f_permute(&[0, 2, 1])?;
        }

        Ok(all_layer_outputs)
    }

    fn n_layers(&self) -> i64 {
        self.layers.len() as i64 + 1
    }
}

#[cfg(feature = "model-tests")]
#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::convert::TryInto;

    use approx::assert_abs_diff_eq;
    use maplit::btreeset;
    use ndarray::{array, ArrayD};
    use syntaxdot_tch_ext::RootExt;
    use tch::nn::VarStore;
    use tch::{Device, Kind, Tensor};

    use super::SqueezeBertEncoder;
    use crate::activations::Activation;
    use crate::models::bert::{BertConfig, BertEmbeddings};
    use crate::models::squeeze_bert::SqueezeBertConfig;
    use crate::models::Encoder;
    use crate::module::FallibleModuleT;

    const SQUEEZEBERT_UNCASED: &str = env!("SQUEEZEBERT_UNCASED");

    fn squeezebert_uncased_config() -> SqueezeBertConfig {
        SqueezeBertConfig {
            attention_probs_dropout_prob: 0.1,
            embedding_size: 768,
            hidden_act: Activation::Gelu,
            hidden_dropout_prob: 0.1,
            hidden_size: 768,
            initializer_range: 0.02,
            intermediate_size: 3072,
            layer_norm_eps: 1e-12,
            max_position_embeddings: 512,
            num_attention_heads: 12,
            num_hidden_layers: 12,
            type_vocab_size: 2,
            vocab_size: 30528,
            q_groups: 4,
            k_groups: 4,
            v_groups: 4,
            post_attention_groups: 1,
            intermediate_groups: 4,
            output_groups: 4,
        }
    }

    fn layer_variables() -> BTreeSet<String> {
        btreeset![
            "post_attention.conv1d.bias".to_string(),
            "post_attention.conv1d.weight".to_string(),
            "post_attention.layer_norm.bias".to_string(),
            "post_attention.layer_norm.weight".to_string(),
            "attention.key.bias".to_string(),
            "attention.key.weight".to_string(),
            "attention.query.bias".to_string(),
            "attention.query.weight".to_string(),
            "attention.value.bias".to_string(),
            "attention.value.weight".to_string(),
            "intermediate.conv1d.bias".to_string(),
            "intermediate.conv1d.weight".to_string(),
            "output.conv1d.bias".to_string(),
            "output.conv1d.weight".to_string(),
            "output.layer_norm.bias".to_string(),
            "output.layer_norm.weight".to_string()
        ]
    }

    fn seqlen_to_mask(seq_lens: Tensor, max_len: i64) -> Tensor {
        let batch_size = seq_lens.size()[0];
        Tensor::arange(max_len, (Kind::Int, Device::Cpu))
            // Construct a matrix [batch_size, max_len] where each row
            // is 0..(max_len - 1).
            .repeat(&[batch_size])
            .view_(&[batch_size, max_len])
            // Time steps less than the length in seq_lens are active.
            .lt_tensor(&seq_lens.unsqueeze(1))
    }

    fn varstore_variables(vs: &VarStore) -> BTreeSet<String> {
        vs.variables()
            .into_iter()
            .map(|(k, _)| k)
            .collect::<BTreeSet<_>>()
    }

    #[test]
    fn squeeze_bert_encoder() {
        let config = squeezebert_uncased_config();
        let bert_config: BertConfig = (&config).into();

        let mut vs = VarStore::new(Device::Cpu);
        let root = vs.root_ext(|_| 0);

        let embeddings = BertEmbeddings::new(root.sub("embeddings"), &bert_config).unwrap();
        let encoder = SqueezeBertEncoder::new(root.sub("encoder"), &config).unwrap();

        vs.load(SQUEEZEBERT_UNCASED).unwrap();

        // Word pieces of: Did the AWO embezzle donations ?
        let pieces =
            Tensor::of_slice(&[2106i64, 1996, 22091, 2080, 7861, 4783, 17644, 11440, 1029])
                .reshape(&[1, 9]);

        let embeddings = embeddings.forward_t(&pieces, false).unwrap();

        let all_hidden_states = encoder.encode(&embeddings, None, false).unwrap();

        let summed_last_hidden =
            all_hidden_states
                .last()
                .unwrap()
                .output()
                .sum_dim_intlist(&[-1], false, Kind::Float);

        let sums: ArrayD<f32> = (&summed_last_hidden).try_into().unwrap();

        assert_abs_diff_eq!(
            sums,
            (array![[
                -0.3894, -0.4608, -0.4127, -0.1656, -0.3927, -0.1952, -0.4998, -0.2477, -0.1676
            ]])
            .into_dyn(),
            epsilon = 1e-4
        );
    }

    #[test]
    fn squeeze_bert_encoder_attention_mask() {
        let config = squeezebert_uncased_config();
        let bert_config: BertConfig = (&config).into();

        let mut vs = VarStore::new(Device::Cpu);
        let root = vs.root_ext(|_| 0);

        let embeddings = BertEmbeddings::new(root.sub("embeddings"), &bert_config).unwrap();
        let encoder = SqueezeBertEncoder::new(root.sub("encoder"), &config).unwrap();

        vs.load(SQUEEZEBERT_UNCASED).unwrap();

        // Word pieces of: Did the AWO embezzle donations ?
        // Add some padding to simulate inactive time steps.
        let pieces = Tensor::of_slice(&[
            2106i64, 1996, 22091, 2080, 7861, 4783, 17644, 11440, 1029, 0, 0, 0, 0, 0,
        ])
        .reshape(&[1, 14]);

        let attention_mask = seqlen_to_mask(Tensor::of_slice(&[9]), pieces.size()[1]);

        let embeddings = embeddings.forward_t(&pieces, false).unwrap();

        let all_hidden_states = encoder
            .encode(&embeddings, Some(&attention_mask), false)
            .unwrap();

        let summed_last_hidden = all_hidden_states
            .last()
            .unwrap()
            .output()
            .slice(-2, 0, 9, 1)
            .sum_dim_intlist(&[-1], false, Kind::Float);

        let sums: ArrayD<f32> = (&summed_last_hidden).try_into().unwrap();

        assert_abs_diff_eq!(
            sums,
            (array![[
                -0.3894, -0.4608, -0.4127, -0.1656, -0.3927, -0.1952, -0.4998, -0.2477, -0.1676
            ]])
            .into_dyn(),
            epsilon = 1e-4
        );
    }

    #[test]
    fn squeeze_bert_encoder_names_and_shapes() {
        // Verify that the encoders's names and shapes are correct.
        let config = squeezebert_uncased_config();

        let vs = VarStore::new(Device::Cpu);
        let root = vs.root_ext(|_| 0);

        let _encoder = SqueezeBertEncoder::new(root, &config).unwrap();

        let variables = varstore_variables(&vs);

        let mut encoder_variables = BTreeSet::new();
        let layer_variables = layer_variables();
        for idx in 0..config.num_hidden_layers {
            for layer_variable in &layer_variables {
                encoder_variables.insert(format!("layer_{}.{}", idx, layer_variable));
            }
        }

        assert_eq!(variables, encoder_variables);
    }
}
