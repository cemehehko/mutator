#!/usr/bin/env python3
"""Эталоны для тестов разбора: файлы пишет и читает python-tll — тот же разбор,
что у tll-read.

    scripts/golden.py            # пересоздать tests/data/*.dat и *.json
    scripts/golden.py FILE OUT   # эталон для любого файла (первые и редкие сообщения)
    scripts/golden.py --patch    # только эталоны правок tests/data/patch.json, stream.json

`patch.json` — правки `PATCH` поверх `plain.dat`, прогнанные настоящим tll-patch
(`patch+file://`, модуль `TLL_PATCH`, по умолчанию `tll-patch`): у каждого
сообщения — правки и тело после них. Им сверяется свой кодировщик правок.

`stream.json` — состав потока `STREAM` (удаление со сдвигом и дырой, смена типа,
вставки в живой seq и в дыру, правки полей после сдвига) поверх `plain.dat` и
`sparse.dat`, прогнанный тем же tll-patch: операции и весь поток после них
(тела — у первых 40 сообщений).

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
import time

from tll.channel import Context
from tll.chrono import Duration, TimePoint
from tll.processor import Loop

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


# Правки поверх plain.dat: (seq, тип, путь, значение) — все виды скаляров,
# offset-строка, вариант union и поле, которого не было по pmap (seq 3, opt.b).
PATCH = [
    (0, 'Order', 'header.order_id', '777'),
    (0, 'Order', 'header.status', 'Wait'),
    (0, 'Order', 'header.ts', '2026-01-02T03:04:05.123456'),
    (0, 'Order', 'symbol', 'NEWSYM'),
    (0, 'Order', 'side', 'Sell'),
    (0, 'Order', 'price', '12.5'),
    (0, 'Order', 'amt', '1.2E+3'),
    (0, 'Order', 'ratio', '0.75'),
    (0, 'Order', 'ttl', '1500ms'),
    (0, 'Order', 'u8', '7'),
    (0, 'Order', 'u64', '18446744073709551615'),
    (0, 'Order', 'i16', '-32768'),
    (0, 'Order', 'raw', 'wxyz'),
    (0, 'Order', 'comment', 'новый комментарий'),
    (0, 'Order', 'u.text', 'вариант'),
    (1, 'Order', 'u.num', '-5'),
    (1, 'Order', 'comment', ''),
    (3, 'Cancel', 'opt.b', '-9'),
    (3, 'Cancel', 'header.status', 'OK'),
]


def patch():
    """Эталон правок: plain.dat через tll-patch с командами `Field` из PATCH."""
    by_seq = {}
    for seq, name, path, value in PATCH:
        by_seq.setdefault((seq, name), []).append((path, value))
    yaml = ''
    for (seq, name), fields in by_seq.items():
        yaml += f"- cmd: Field\n  seq: {seq}\n  name: '{name}'\n  fields:\n"
        yaml += ''.join(f"    {p}: '{v}'\n" for p, v in fields)
    tmp = DATA / 'patch.edits.yaml'
    tmp.write_text(yaml)
    try:
        ctx = Context()
        ctx.load(os.environ.get('TLL_PATCH', 'tll-patch'))
        c = ctx.Channel(f'patch+file://{DATA / "plain.dat"}', name='r', dump='no', autoclose='yes', patch=str(tmp))
        rows = []
        seqs = {seq for seq, _ in by_seq}

        def cb(ch, m):
            if m.type != m.Type.Data or m.seq not in seqs:
                return
            msg = ch.scheme.unpack(m)
            edits = {p: v for (s, n), f in by_seq.items() if s == m.seq for p, v in f}
            rows.append({'seq': m.seq, 'msgid': m.msgid, 'name': msg.SCHEME.name, 'edits': edits,
                         'data': jsonable(msg.as_dict())})

        c.callback_add(cb)
        # Префикс сам дочерний file-канал не крутит — опрос через Loop, как у
        # run.py tll-patch; до первого Closed, с пределом по времени.
        loop = Loop(config={'poll': 'no'})
        loop.add(c)
        c.open()
        deadline = time.time() + 30
        while c.state not in (c.State.Closed, c.State.Error) and time.time() < deadline:
            loop.step(0.01)
        assert c.state == c.State.Closed, f'tll-patch: {c.state}'
        assert len(rows) == len(seqs), f'tll-patch отдал {len(rows)} из {len(seqs)}'
    finally:
        tmp.unlink()
    (DATA / 'patch.json').write_text(json.dumps(rows, ensure_ascii=False, indent=1) + '\n')
    return len(rows)


# Тело с нуля: все виды полей, списки всех offset-ptr, массив, union. Многобитное
# поле битов python-tll показывает bool — `lvl` здесь 1 (`ioc|16`).
FULL = {
    'header.ts': '2026-01-02T03:04:05', 'header.status': 'Wait', 'header.order_id': '9',
    'symbol': 'XX', 'side': 'Sell', 'flags': 'ioc|16', 'price': '1.5', 'amt': '2.5',
    'ratio': '0.25', 'ttl': '10ms', 'u8': '1', 'u64': '18446744073709551615', 'raw': 'ab',
    'arr[0]': '5', 'arr[1]': '6', 'comment': 'новое',
    'legs[0].symbol': 'A', 'legs[0].qty': '1', 'legs[1].symbol': 'B', 'legs[1].qty': '2',
    'short[0].symbol': 'S', 'short[0].qty': '3', 'long[0].symbol': 'LL', 'long[0].qty': '4',
    'tags[0]': 'x', 'tags[1]': 'yy', 'u.text': 'ins',
}

# Состав потока: файл → операции. `from` — тип сообщения в файле.
STREAM = {
    'plain': [
        {'op': 'remove', 'seq': 2, 'from': 'Cancel'},
        {'op': 'clear', 'seq': 4, 'from': 'Ping'},
        {'op': 'retype', 'seq': 5, 'from': 'Order', 'name': 'Ping', 'body': {'n': '55'}},
        {'op': 'field', 'seq': 5, 'name': 'Ping', 'fields': {'n': '56'}},
        {'op': 'retype', 'seq': 7, 'from': 'Cancel', 'name': 'Order', 'body': FULL},
        {'op': 'insert', 'seq': 9, 'name': 'Cancel', 'occupied': True,
         'body': {'header.order_id': '1', 'opt.a': '7', 'opt.b': '8'}},
        {'op': 'insert', 'seq': 9, 'name': 'Ping', 'occupied': True, 'body': {'n': '1'}},
        {'op': 'field', 'seq': 10, 'name': 'Order', 'fields': {'price': '3.25', 'comment': 'c'}},
        {'op': 'insert', 'seq': 12, 'name': 'Order', 'occupied': True, 'body': FULL},
        {'op': 'remove', 'seq': 13, 'from': 'Cancel'},
        {'op': 'remove', 'seq': 14, 'from': 'Ping'},
        {'op': 'field', 'seq': 20, 'name': 'Order', 'fields': {'header.status': 'Fail'}},
    ],
    'sparse': [
        {'op': 'insert', 'seq': 20, 'name': 'Ping', 'occupied': False, 'body': {'n': '20'}},
        {'op': 'insert', 'seq': 25, 'name': 'Ping', 'occupied': True, 'body': {'n': '25'}},
        {'op': 'clear', 'seq': 35, 'from': 'Cancel'},
        {'op': 'remove', 'seq': 45, 'from': 'Ping'},
        {'op': 'field', 'seq': 55, 'name': 'Order', 'fields': {'price': '7'}},
    ],
}


def nest(body):
    """Плоские пути (`legs[0].qty`) → вложенные словари и списки по номерам."""
    root = {}
    for path, value in body.items():
        node, steps = root, []
        for part in path.split('.'):
            name, _, rest = part.partition('[')
            steps.append(name)
            steps += [int(i) for i in rest.rstrip(']').split('][') if i] if rest else []
        for k, nxt in zip(steps, steps[1:] + [None]):
            fresh = [] if isinstance(nxt, int) else {}
            if isinstance(k, int):
                while len(node) <= k:
                    node.append(None)
                if nxt is None:
                    node[k] = value
                else:
                    node[k] = node[k] if node[k] is not None else fresh
                    node = node[k]
            else:
                if nxt is None:
                    node[k] = value
                else:
                    node = node.setdefault(k, fresh)
    return root


def stream():
    """Эталон состава потока: каждый файл STREAM через tll-patch целиком."""
    out = []
    for name, ops in STREAM.items():
        cmds = []
        for o in ops:
            seq = o['seq']
            if o['op'] in ('remove', 'clear'):
                cmds.append({'cmd': o['op'].capitalize(), 'seq': seq})
            elif o['op'] == 'field':
                cmds.append({'cmd': 'Field', 'seq': seq, 'name': o['name'], 'fields': o['fields']})
            elif o['op'] == 'retype':
                body = dict(o['body'])
                for f in ops:
                    if f['op'] == 'field' and f['seq'] == seq:
                        body.update(f['fields'])
                cmds.append({'cmd': 'Update', 'seq': seq, 'name': o['name'], 'fields': nest(body)})
            else:
                cmds.append({'cmd': 'Insert', 'seq': seq, 'name': o['name'], 'fields': nest(o['body'])})
        retyped = {o['seq'] for o in ops if o['op'] == 'retype'}
        cmds = [c for c in cmds if not (c['cmd'] == 'Field' and c['seq'] in retyped)]
        tmp = DATA / f'{name}.stream.yaml'
        # JSON — тоже yaml: вложенные тела без своего эмиттера.
        tmp.write_text(''.join(f'- {json.dumps(c, ensure_ascii=False)}\n' for c in cmds))
        try:
            rows = read_patched(DATA / f'{name}.dat', tmp)
        finally:
            tmp.unlink()
        # Тела — у начала потока, где правки; дальше хватает seq и типа.
        for r in rows[40:]:
            del r['data']
        out.append({'file': f'{name}.dat', 'ops': ops, 'out': rows})
    (DATA / 'stream.json').write_text(json.dumps(out, ensure_ascii=False, indent=1) + '\n')
    return sum(len(x['out']) for x in out)


def read_patched(path, yaml):
    """Весь поток файла через tll-patch: seq, тип и тело каждого сообщения."""
    ctx = Context()
    ctx.load(os.environ.get('TLL_PATCH', 'tll-patch'))
    c = ctx.Channel(f'patch+file://{path}', name='r', dump='no', autoclose='yes', patch=str(yaml))
    rows = []

    def cb(ch, m):
        if m.type != m.Type.Data:
            return
        msg = ch.scheme.unpack(m)
        rows.append({'seq': m.seq, 'msgid': m.msgid, 'name': msg.SCHEME.name, 'data': jsonable(msg.as_dict())})

    c.callback_add(cb)
    loop = Loop(config={'poll': 'no'})
    loop.add(c)
    c.open()
    deadline = time.time() + 30
    while c.state not in (c.State.Closed, c.State.Error) and time.time() < deadline:
        loop.step(0.01)
    assert c.state == c.State.Closed, f'tll-patch: {c.state}'
    return rows


def main():
    if sys.argv[1:] == ['--patch']:
        print('patch', patch())
        print('stream', stream())
        return
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
    print('patch', patch())
    print('stream', stream())


if __name__ == '__main__':
    main()
