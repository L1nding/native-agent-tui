"""Compare an explicitly exported Codex schema bundle with the reviewed baseline."""
import argparse
import hashlib
import json
from pathlib import Path
import sys

REPO = Path(__file__).resolve().parents[1]
FIXTURES = REPO / 'tests/fixtures/codex-0.161.0'


def fingerprint(value):
    canonical = json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(',', ':')).encode('utf-8')
    return hashlib.sha256(canonical).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--schema-dir', type=Path, help='Output of the pinned CLI generate-json-schema --experimental')
    args = parser.parse_args()
    manifest = json.loads((FIXTURES / 'schema-manifest.json').read_text(encoding='utf-8'))
    retained = [
        'v1/InitializeResponse.json',
        'v2/CommandExecResponse.json',
        'v2/SkillsListParams.json',
        'v2/SkillsListResponse.json',
        'v2/SkillsChangedNotification.json',
        'v2/ItemCompletedNotification.json',
        'v2/FileChangePatchUpdatedNotification.json',
        'v2/CommandExecutionOutputDeltaNotification.json',
    ]
    for name in retained:
        schema = json.loads((FIXTURES / Path(name).name).read_text(encoding='utf-8'))
        if fingerprint(schema) != manifest['schemas'][name]['sha256']:
            raise ValueError(f'Retained source schema changed: {name}')
    response = json.loads((FIXTURES / 'initialize.json').read_text(encoding='utf-8'))['result']
    schema = json.loads((FIXTURES / 'InitializeResponse.json').read_text(encoding='utf-8'))
    for field in schema['required']:
        if field not in response or not isinstance(response[field], str) or not response[field]:
            raise ValueError('Initialize fixture does not match the retained source schema')
    transcript = [json.loads(line) for line in (FIXTURES / 'startup.jsonl').read_text(encoding='utf-8').splitlines()]
    if transcript[0]['result'] != response:
        raise ValueError('Startup transcript and initialize fixture disagree')
    if args.schema_dir:
        for name, expected in manifest['schemas'].items():
            schema = json.loads((args.schema_dir / name).read_text(encoding='utf-8'))
            if fingerprint(schema) != expected['sha256'] or schema.get('required', []) != expected['required']:
                raise ValueError(f'Exported schema changed; manual compatibility review required: {name}')
        print(f'Pinned protocol schemas: {len(manifest["schemas"])} exported fingerprints match the reviewed baseline')
    else:
        print('Pinned protocol fixtures: retained schemas and initialize/startup fixture match (no external CLI queried)')


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, KeyError, IndexError, TypeError):
        print('Protocol schema check failed; review the pinned fixture and explicit schema export.', file=sys.stderr)
        sys.exit(1)
