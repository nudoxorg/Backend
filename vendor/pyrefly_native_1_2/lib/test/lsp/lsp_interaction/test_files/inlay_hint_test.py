# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is licensed under the MIT license found in the
# LICENSE file in the root directory of this source tree.


def no_return_annot():
    _ = (1, 2)  # no inlay hint here
    return (1, 2)


result = no_return_annot()


async def foo():
    return 0
