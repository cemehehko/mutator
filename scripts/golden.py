#!/usr/bin/env python3
"""Эталоны для тестов разбора: файлы пишет и читает python-tll — тот же разбор,
что у tll-read.

    scripts/golden.py            # пересоздать tests/data/*.dat и *.json
    scripts/golden.py FILE OUT   # эталон для любого файла (первые и редкие сообщения)

JSON: [{seq, msgid, name, data}], где data — дерево as_dict(), а скаляры —
строки в том виде, в каком их печатает tll-read (enum — имя, Decimal и время — str).
Рядом — `*.yaml`: вывод самого tll-read (`TLL_READ`, по умолчанию
`../tll-lua/tll-read`), с ним байт в байт сверяется `--dump`.
"""

import decimal
import enum
import json
import os
import pathlib
import subprocess
import sys

from tll.channel import Context
from tll.chrono import Duration, TimePoint

ROOT = pathlib.Path(__file__).resolve().parent.parent
DATA = ROOT / 'tests' / 'data'

SCHEME = '''yamls://
- name: Leg
  fields:
    - {name: symbol, type: byte8, options.type: string}
    - {name: qty, type: int32}
- name: Opt
  options.defaults.optional: true
  fields:
    - {name: pmap, type: uint8, options.pmap: true}
    - {name: a, type: int32}
    - {name: b, type: int32, options.optional: true}
- name: Head
  fields:
    - {name: ts, type: int64, options.type: time_point, options.resolution: us}
    - {name: status, type: Status}
    - {name: order_id, type: int64}
  enums:
    Status: {type: int8, enum: {OK: 0, Fail: 1, Wait: 2}}
- name: Order
  id: 10
  enums:
    Side: {type: uint8, enum: {Buy: 1, Sell: 2}}
  bits:
    Flags: {type: uint16, bits: [{name: ioc, offset: 0}, {name: fok, offset: 1}, {name: lvl, offset: 4, size: 3}]}
  fields:
    - {name: header, type: Head}
    - {name: symbol, type: byte17, options.type: string}
    - {name: side, type: Side}
    - {name: flags, type: Flags}
    - {name: price, type: int64, options.type: fixed8}
    - {name: amt, type: decimal128}
    - {name: ratio, type: double}
    - {name: ttl, type: int32, options.type: duration, options.resolution: ms}
    - {name: u8, type: uint8}
    - {name: u16, type: uint16}
    - {name: u32, type: uint32}
    - {name: u64, type: uint64}
    - {name: i16, type: int16}
    - {name: raw, type: byte4}
    - {name: arr, type: 'int16[3]'}
    - {name: comment, type: string}
    - {name: legs, type: '*Leg'}
    - {name: short, type: '*Leg', list-options.offset-ptr-type: legacy-short}
    - {name: long, type: '*Leg', list-options.offset-ptr-type: legacy-long}
    - {name: tags, type: '*string'}
    - {name: u, type: union, union: [{name: num, type: int64}, {name: text, type: string}]}
- name: Cancel
  id: 20
  fields:
    - {name: header, type: Head}
    - {name: opt, type: Opt}
- name: Ping
  id: 30
  fields:
    - {name: n, type: uint32}
'''


def message(i):
    """Сообщение номер i: тип и поля варьируются, чтобы покрыть разные значения."""
    base = 1790084487 * 1000000
    head = {'ts': TimePoint(base + i * 1001, 'us'), 'status': ['OK', 'Fail', 'Wait'][i % 3], 'order_id': 1000 + i // 2}
    k = i % 5
    if k in (0, 1):
        return 'Order', {
            'header': head,
            'symbol': ['SBER', 'GAZP', ''][i % 3],
            'side': ['Buy', 'Sell'][i % 2],
            'flags': {'ioc': bool(i % 2), 'fok': bool(i % 3 == 0), 'lvl': i % 8},
            'price': decimal.Decimal(f'{100 + i}.{i % 100:02}'),
            'amt': decimal.Decimal(['0', '3000000.00', '-1.5E+3', '0.000001'][i % 4]),
            'ratio': [0.5, 1.0, -2.25][i % 3],
            'ttl': Duration(i * 10, 'ms'),
            'u8': 255 - i % 256, 'u16': 65535 - i, 'u32': 4000000000 + i, 'u64': 2**63 + i, 'i16': -i,
            'raw': bytes([65 + i % 26, 0, 1, 255]) if i % 2 else b'ab\x00\x00',
            'arr': list(range(i % 4)),
            'comment': 'привет' if i % 4 == 0 else f'c{i}',
            'legs': [{'symbol': f'L{j}', 'qty': j * i} for j in range(i % 3)],
            'short': [{'symbol': 'S', 'qty': i}],
            'long': [{'symbol': 'LL', 'qty': -i}] * (i % 2),
            'tags': [f't{j}' for j in range(i % 3)],
            'u': {'num': i} if i % 2 else {'text': f'u{i}'},
        }
    if k in (2, 3):
        opt = {'a': i} if i % 2 else {'a': i, 'b': -i}
        return 'Cancel', {'header': head, 'opt': opt}
    return 'Ping', {'n': i}


def jsonable(v):
    """Скаляры — строками, как их печатает tll-read."""
    if isinstance(v, dict):
        return {k: jsonable(x) for k, x in v.items()}
    if isinstance(v, (list, tuple)) and not hasattr(v, 'type'):
        return [jsonable(x) for x in v]
    if isinstance(v, enum.Enum):
        return v.name
    if isinstance(v, bool):
        return str(int(v))
    if isinstance(v, (bytes, bytearray)):
        b = bytes(v)
        if all(x < 0x80 for x in b):
            return ''.join('\\0' if c == 0 else chr(c) for c in b)
        return b.hex()
    if isinstance(v, float):
        return repr(v)
    if hasattr(v, 'type') and hasattr(v, 'value'):  # union
        return {v.type: jsonable(v.value)}
    if hasattr(v, 'BITS'):  # bits
        return '|'.join(n if getattr(v, n) is True else f'{n}={int(getattr(v, n))}'
                        for n in v.BITS if getattr(v, n))
    return str(v)


def write(path, compression, block, count, seq0=0, step=1):
    ctx = Context()
    c = ctx.Channel(f'file://{path}', dir='w', scheme=SCHEME, compression=compression, block=block, name='w', dump='no')
    c.open(overwrite='yes')
    for i in range(count):
        name, body = message(i)
        c.post(body, name=name, seq=seq0 + i * step)
    c.close()


def dump(path, out, limit=None):
    ctx = Context()
    c = ctx.Channel(f'file://{path}', name='r', dump='no', autoclose='yes')
    rows = []

    def cb(ch, m):
        if m.type != m.Type.Data:
            return
        n = len(rows)
        if limit is None or n < limit or m.seq % 997 == 0:
            msg = ch.scheme.unpack(m)
            rows.append({'seq': m.seq, 'msgid': m.msgid, 'name': msg.SCHEME.name, 'data': jsonable(msg.as_dict())})

    c.callback_add(cb)
    c.open()
    while c.state == c.State.Active:
        c.process()
    pathlib.Path(out).write_text(json.dumps(rows, ensure_ascii=False, indent=1) + '\n')
    return len(rows)


def main():
    if len(sys.argv) == 3:
        print(dump(sys.argv[1], sys.argv[2], limit=2000))
        return
    DATA.mkdir(parents=True, exist_ok=True)
    # Без сжатия, LZ4 и разреженные seq; блок 16 КБ (кратно странице macOS) — много блоков на малом файле.
    for name, comp, count, seq0, step in [('plain', 'none', 400, 0, 1), ('lz4', 'lz4', 1500, 100, 1), ('sparse', 'lz4', 400, 5, 10)]:
        dat = DATA / f'{name}.dat'
        write(dat, comp, '16kb', count, seq0, step)
        print(name, dump(dat, DATA / f'{name}.json'))
        tll_read = os.environ.get('TLL_READ', str(ROOT.parent / 'tll-lua' / 'tll-read'))
        out = subprocess.run([sys.executable, tll_read, str(dat)], check=True, capture_output=True).stdout
        (DATA / f'{name}.yaml').write_bytes(out)


if __name__ == '__main__':
    main()
