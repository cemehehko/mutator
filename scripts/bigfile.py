#!/usr/bin/env python3
"""Большой файл для замеров: сообщения образца, повторённые до нужного объёма.

    scripts/bigfile.py SAMPLE.dat OUT.dat [ГБ=2]

Схема и сжатие — как у образца (LZ4), seq идут подряд. Потом:

    cargo run --release --example bench -- OUT.dat
"""

import os
import sys

from tll.channel import Context


def main():
    src, out = sys.argv[1], sys.argv[2]
    gb = float(sys.argv[3]) if len(sys.argv) > 3 else 2.0
    ctx = Context()
    r = ctx.Channel(f'file://{src}', name='src', dump='no', autoclose='yes')
    msgs = []
    r.callback_add(lambda c, m: msgs.append((m.msgid, bytes(m.data))) if m.type == m.Type.Data else None)
    r.open()
    scheme = r.scheme.dump('yamls+gz')  # после закрытия канала схемы уже нет
    while r.state == r.State.Active:
        r.process()
    w = ctx.Channel(f'file://{out}', dir='w', name='out', dump='no', compression='lz4', block='1mb', scheme=scheme)
    w.open(overwrite='yes')
    rounds = max(1, int(gb * 1e9 / max(os.path.getsize(src), 1)))
    seq = 0
    for i in range(rounds):
        for msgid, data in msgs:
            w.post(data, msgid=msgid, seq=seq)
            seq += 1
        print(f'\r{i + 1}/{rounds}', end='', flush=True)
    w.close()
    print(f'\n{seq} сообщений')


if __name__ == '__main__':
    main()
