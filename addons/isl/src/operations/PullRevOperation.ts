/**
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

import {Operation} from './Operation';

export class PullRevOperation extends Operation {
  static opName = 'PullRev';

  constructor(private rev: string) {
    super('PullRevOperation');
  }

  getArgs() {
    return ['pull', '--rev', this.rev];
  }
}
