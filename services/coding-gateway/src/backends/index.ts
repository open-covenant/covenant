import type { BackendId, CodingBackend } from '../types.js';
import { AnthropicBackend } from './anthropic.js';
import { OpenaiBackend } from './openai.js';
import { UsePodBackend } from './usepod.js';

/** Select a coding backend by id. Anthropic is the default brain; OpenAI is the
 *  selectable second brain for comparison runs. */
export function selectBackend(id: BackendId = 'anthropic'): CodingBackend {
  switch (id) {
    case 'anthropic':
      return new AnthropicBackend();
    case 'openai':
      return new OpenaiBackend();
    case 'usepod':
      return new UsePodBackend();
    default:
      throw new Error(`unknown backend: ${id satisfies never}`);
  }
}
