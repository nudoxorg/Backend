"""Independent BQPC/1 layout oracle. No runtime, authority or native admission."""
import base64
import copy
import hashlib
import json
from pathlib import Path
import struct
import sys
import zlib

MAGIC = b'BQPC\x00\x01'
CAP = 256 * 1024
SCHEMAS = dict(enumerate(('actor', 'package', 'symbol', 'object', 'view_recipe', 'view_version', 'branch', 'log', 'document', 'name', 'outline', 'semantic_shape_source', 'semantic_shape_batch', 'view_relation'), 1))
SCHEMA_TAGS = {value: key for key, value in SCHEMAS.items()}
FIELDS = {
    1: ('key', [('schema', 'schema'), ('id', 'digest'), ('value', 'text')]),
    2: ('key_bytes', [('schema', 'schema'), ('id', 'digest'), ('value', 'bytes')]),
    3: ('key_commitment', [('schema', 'schema'), ('id', 'digest')]),
    4: ('row_identity', [('schema', 'schema'), ('id', 'digest'), ('preimage', 'text')]),
    5: ('version', [('schema', 'schema'), ('id', 'digest'), ('value', 'bytes')]),
    6: ('root', [('schema', 'schema'), ('id', 'digest'), ('canonical', 'bytes')]),
    7: ('root_commitment', [('schema', 'schema'), ('id', 'digest')]),
    8: ('intent', [('id', 'digest'), ('token', 'text'), ('payload', 'bytes')]),
    9: ('delta', [('schema', 'schema'), ('id', 'digest'), ('base', 'digest'), ('target', 'digest'), ('changes', 'bytes')]),
    10: ('cursor', [('recipe', 'digest'), ('version', 'digest'), ('branch', 'digest'), ('log', 'digest'), ('schema', 'u16'), ('root', 'digest'), ('sequence', 'u64')]),
    11: ('coverage', [('scope', 'digest'), ('observed', 'digest'), ('producer', 'digest'), ('context', 'digest'), ('evidence', 'bytes')]),
}
FAMILY_TAGS = {value[0]: key for key, value in FIELDS.items()}
dump = lambda value: json.dumps(value, ensure_ascii=False, separators=(',', ':')).encode()
length = lambda count: struct.pack('>I', count)

def put_blob(value):
    return length(len(value)) + value

def pack(value):
    assert len(dump(value)) <= CAP
    header = {key: value[key] for key in ('version', 'request_id', 'command')}
    frame = bytearray(MAGIC + put_blob(dump(header)))
    certificate = value.get('certificate')
    frame.append(int(certificate is not None))
    if certificate is not None:
        assert set(certificate) == {'claims'}
        frame.extend(length(len(certificate['claims'])))
        for claim in certificate['claims']:
            tag = FAMILY_TAGS[claim['kind']]
            fields = FIELDS[tag][1]
            assert set(claim) == {'kind', 'data'}
            assert set(claim['data']) == {name for name, _ in fields}
            frame.append(tag)
            for name, kind in fields:
                field = claim['data'][name]
                if kind == 'schema': frame.append(SCHEMA_TAGS[field])
                elif kind == 'digest':
                    assert len(field) == 64 and all(c in '0123456789abcdef' for c in field)
                    frame.extend(bytes.fromhex(field))
                elif kind == 'bytes': frame.extend(put_blob(bytes(field)))
                elif kind == 'text': frame.extend(put_blob(field.encode()))
                elif kind == 'u16': frame.extend(struct.pack('>H', field))
                elif kind == 'u64': frame.extend(struct.pack('>Q', field))
    assert len(frame) <= CAP
    return bytes(frame)

class Reader:
    def __init__(self, data): self.data, self.position = data, 0
    def take(self, count):
        if count > len(self.data) - self.position: raise ValueError('truncated field')
        value = self.data[self.position:self.position + count]
        self.position += count
        return value
    def number(self, format, size): return struct.unpack(format, self.take(size))[0]
    def byte(self): return self.take(1)[0]
    def blob(self): return self.take(self.number('>I', 4))

def unpack(frame):
    if len(frame) > CAP: raise ValueError('physical cap')
    reader = Reader(frame)
    if reader.take(6) != MAGIC: raise ValueError('unknown version')
    raw_header = reader.blob()
    header = json.loads(raw_header)
    if set(header) != {'version', 'request_id', 'command'} or dump(header) != raw_header: raise ValueError('noncanonical header')
    present = reader.byte()
    if present not in (0, 1): raise ValueError('presence tag')
    certificate = None
    if present:
        count = reader.number('>I', 4)
        if count > (len(frame) - reader.position) // 34: raise ValueError('claim count')
        claims = []
        for _ in range(count):
            tag = reader.byte()
            if tag not in FIELDS: raise ValueError('claim tag')
            family, fields = FIELDS[tag]
            data = {}
            for name, kind in fields:
                if kind == 'schema':
                    schema = reader.byte()
                    if schema not in SCHEMAS: raise ValueError('schema tag')
                    field = SCHEMAS[schema]
                elif kind == 'digest': field = reader.take(32).hex()
                elif kind == 'bytes': field = list(reader.blob())
                elif kind == 'text': field = reader.blob().decode()
                elif kind == 'u16': field = reader.number('>H', 2)
                elif kind == 'u64': field = reader.number('>Q', 8)
                data[name] = field
            claims.append({'kind': family, 'data': data})
        certificate = {'claims': claims}
    if reader.position != len(frame): raise ValueError('trailing bytes')
    value = {**header, 'certificate': certificate}
    if len(dump(value)) > CAP: raise ValueError('expanded cap')
    return value

def run():
    source = Path('/private/tmp/nudox-root-go-current-03/0066-mcp-wire.json')
    raw = source.read_bytes().removesuffix(b'\n')
    assert hashlib.sha256(raw).hexdigest() == 'd55dfef0dd30ace553a18ab73cb21101725643bd24ee180650a07521d62ed4bf'
    response = json.loads(raw)
    cursor = response['result']['structuredContent']['nextCursor']
    signed_body, mac = cursor.rsplit('-', 1)
    expiry, owner = signed_body.removeprefix('mcp2-').split('-', 1)
    assert owner.startswith('pc3-')
    compressed = base64.urlsafe_b64decode(owner[4:] + '=' * (-len(owner[4:]) % 4))
    decoder = zlib.decompressobj()
    original_body = decoder.decompress(compressed, CAP + 1)
    assert len(original_body) <= CAP and decoder.eof and not decoder.unused_data
    command = json.loads(original_body)
    assert dump(command) == original_body
    packed = pack(command)
    restored = unpack(packed)
    assert dump(restored) == original_body and pack(restored) == packed
    fixture_directory = Path(__file__).parents[2] / 'crates/library/wire/command/fixtures'
    fixture_directory.mkdir(exist_ok=True)
    (fixture_directory / 'current-go-graph-continuation.json').write_bytes(original_body)
    (fixture_directory / 'current-go-graph-continuation.bqpc1').write_bytes(packed)
    negative_checks = 0
    for altered in [packed[:end] for end in (0, 5, 9, 128, len(packed) - 1)] + [packed + b'\x00', packed[:5] + b'\x02' + packed[6:]]:
        try: unpack(altered)
        except (ValueError, KeyError): negative_checks += 1
        else: raise AssertionError('malformed frame expanded')
    sys.path.insert(0, '/private/tmp/sol61-ilo-real-cli-mcp-20261011/tiktoken-pinned')
    import tiktoken
    assert tiktoken.__version__ == '0.11.0'
    encoding = tiktoken.get_encoding('cl100k_base')
    artifact = {'mergeable-ranks': {key.hex(): value for key, value in sorted(encoding._mergeable_ranks.items())}, 'special-tokens': dict(sorted(encoding._special_tokens.items())), 'pattern': encoding._pat_str}
    artifact_sha = hashlib.sha256(json.dumps(artifact, sort_keys=True, separators=(',', ':')).encode()).hexdigest()
    assert artifact_sha == '8613f818e5af318d868379772ce5234e3a3e8519b6c611d94cf708fdfc580fbc'
    compressed_binary = zlib.compress(packed)
    binary_owner = 'pc4-' + base64.urlsafe_b64encode(compressed_binary).decode().rstrip('=')
    # Accounting model only: reusing a MAC does NOT authenticate a new payload.
    projected = copy.deepcopy(response)
    body = projected['result']['structuredContent']
    for row in body['records']:
        for field in ('project', 'key'): row['identity'].pop(field, None)
    projected['result']['content'][0]['text'] = (
        response['result']['content'][0]['text'].splitlines()[0] + '\n'
        + f"{len(body['records'])} row(s); complete records and native operands are in `structuredContent.records`.\n"
        + 'Continue `backend.graph` with the same arguments; copy `structuredContent.nextCursor` to `arguments.cursor`.'
    )
    body['nextCursor'] = f'mcp2-{expiry}-{binary_owner}-{mac}'
    for _ in range(12):
        measured = len(dump(body)); body['budget']['bytes'] = measured; body['budget']['estimatedTokens'] = (measured + 3) // 4
        if len(dump(body)) == measured: break
    wire_budget = projected['result']['_meta']['backend/wireBudget']
    for _ in range(12):
        measured = len(dump(projected)) + 1; wire_budget['bytes'] = measured; wire_budget['estimatedTokens'] = (measured + 3) // 4
        if len(dump(projected)) + 1 == measured: break
    modeled = dump(projected)
    report = {
        'scope': 'Source-only transport layout and field-exact token accounting; no valid successor MAC, native execution or owner admission claim.',
        'source': str(source), 'source_sha256': hashlib.sha256(raw).hexdigest(),
        'original_command': {'bytes': len(original_body), 'sha256': hashlib.sha256(original_body).hexdigest(), 'compressed': len(compressed), 'owner_tokens': len(encoding.encode(owner))},
        'binary_command': {'bytes': len(packed), 'sha256': hashlib.sha256(packed).hexdigest(), 'compressed': len(compressed_binary), 'owner_tokens': len(encoding.encode(binary_owner))},
        'claimed_families_in_original_order': [claim['kind'] for claim in command['certificate']['claims']],
        'exact_original_command_bytes_restored': True, 'negative_framing_checks': negative_checks,
        'tokenizer': {'version': tiktoken.__version__, 'encoding': 'cl100k_base', 'artifact_sha256': artifact_sha},
        'modeled_summary_binary_response': {'bytes': len(modeled), 'tokens': len(encoding.encode(modeled.decode())), 'records': len(body['records']), 'within_12000_fixture_target': len(encoding.encode(modeled.decode())) <= 12000},
    }
    output = Path('/private/tmp/binary-certificate-source-evidence-20261011'); output.mkdir(exist_ok=True)
    (output / 'accounting.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report, indent=2))

if __name__ == '__main__': run()
