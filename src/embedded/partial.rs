use super::{Block, ModelDelta};
use std::collections::HashMap;

/// Text received during the current model request, in first-seen block order.
/// Callers clear it once the complete assistant message has been committed.
#[derive(Default)]
pub(super) struct PartialReply {
    blocks: Vec<Block>,
    positions: HashMap<(bool, String), usize>,
}

impl PartialReply {
    pub(super) fn push(&mut self, chunk: &ModelDelta) {
        let (thinking, id, delta) = match chunk {
            ModelDelta::Text { block_id, delta } => (false, block_id, delta),
            ModelDelta::Thinking { block_id, delta } => (true, block_id, delta),
        };
        let index = self
            .positions
            .entry((thinking, id.clone()))
            .or_insert_with(|| {
                let index = self.blocks.len();
                self.blocks.push(if thinking {
                    Block::Thinking {
                        text: String::new(),
                    }
                } else {
                    Block::Text {
                        text: String::new(),
                    }
                });
                index
            });
        match &mut self.blocks[*index] {
            Block::Text { text } | Block::Thinking { text } => text.push_str(delta),
            _ => unreachable!("only streamed text or thinking can enter a partial reply"),
        }
    }

    pub(super) fn into_blocks(self) -> Vec<Block> {
        self.blocks
    }
}
