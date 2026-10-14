#!/usr/bin/env python3
"""Read-only reconciliation of the offline Arcturus rehearsal fixture.

Run with an explicitly chosen local MariaDB socket. No passwords/rows are printed.
This checks core migrated fields, not complete gameplay compatibility.
"""
import argparse
import json
import re
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--mysql', default='mariadb')
parser.add_argument('--socket', required=True)
parser.add_argument('--user', default='root')
parser.add_argument('--source', default='arc_source')
parser.add_argument('--target', default='plus_test')
args = parser.parse_args()
for name in (args.source, args.target):
    if not re.fullmatch(r'[A-Za-z0-9_]+', name):
        parser.error('database names must be simple SQL identifiers')
s, t = '`'+args.source+'`', '`'+args.target+'`'

def scalar(sql):
    result = subprocess.run([args.mysql, '--no-defaults', '--socket='+args.socket,
                             '--user='+args.user, '-NB', '-e', "SET time_zone='+00:00'; "+sql],
                            check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    return int(result.stdout.strip())

# Each query counts discrepancies. Report aggregate counts, never account fields.
checks = {
    'users_count': f'SELECT ABS((SELECT COUNT(*) FROM {s}.users)-(SELECT COUNT(*) FROM {t}.users))',
    'user_identity_and_profile': f'''SELECT COUNT(*) FROM {s}.users a LEFT JOIN {t}.users b ON a.id=b.id
        WHERE b.id IS NULL OR BINARY a.username<>BINARY b.username OR BINARY a.look<>BINARY b.look
        OR BINARY a.motto<>BINARY b.motto OR NOT(a.mail<=>b.mail)
        OR NOT((CASE WHEN LEFT(a.password,4) IN ('$2a$','$2b$') THEN CONCAT('$2y$',SUBSTRING(a.password,5)) ELSE NULLIF(a.password,'') END)<=>b.password)''',
    'credits_including_uncollected_sales': f'''SELECT COUNT(*) FROM {s}.users a LEFT JOIN {t}.users b ON a.id=b.id
        WHERE b.id IS NULL OR b.credits<>a.credits+COALESCE((SELECT SUM(m.price) FROM {s}.marketplace_items m WHERE m.user_id=a.id AND m.state=2),0)''',
    'currencies': f'''SELECT COUNT(*) FROM {s}.users_currency a LEFT JOIN {t}.user_currencies b ON a.user_id=b.user_id AND a.type=b.type
        WHERE b.user_id IS NULL OR a.amount<>b.amount''',
    'room_ownership_and_text': f'''SELECT COUNT(*) FROM {s}.rooms a LEFT JOIN {t}.rooms b ON a.id=b.id
        WHERE b.id IS NULL OR a.owner_id<>b.owner OR BINARY a.name<>BINARY b.caption OR BINARY a.description<>BINARY b.description''',
    'room_entrance_and_wall_height': f'''SELECT COUNT(*) FROM {s}.rooms a JOIN {t}.rooms b ON a.id=b.id
        JOIN {t}.room_models d ON d.id=b.model_name LEFT JOIN {s}.room_models m ON m.name=a.model
        LEFT JOIN {s}.room_models_custom c ON c.id=a.id
        WHERE d.wall_height<>a.wall_height OR d.door_x<>IF(a.override_model='1',c.door_x,m.door_x)
        OR d.door_y<>IF(a.override_model='1',c.door_y,m.door_y) OR d.door_dir<>IF(a.override_model='1',c.door_dir,m.door_dir)''',
    'badge_ownership': f'''SELECT COUNT(*) FROM (SELECT DISTINCT user_id,badge_code FROM {s}.users_badges) a
        LEFT JOIN {t}.user_badges b ON a.user_id=b.user_id AND a.badge_code=b.badge_id WHERE b.user_id IS NULL''',
    'wardrobe_exact': f'''SELECT COUNT(*) FROM {s}.users_wardrobe a LEFT JOIN {t}.user_wardrobe b ON a.user_id=b.user_id AND a.slot_id=b.slot_id
        WHERE b.user_id IS NULL OR BINARY a.look<>BINARY b.look OR UPPER(a.gender)<>b.gender''',
    'directed_friendship_relations': f'''SELECT COUNT(*) FROM {s}.messenger_friendships a LEFT JOIN {t}.messenger_friendships b
        ON a.user_one_id=b.user_one_id AND a.user_two_id=b.user_two_id WHERE b.user_one_id IS NULL OR a.relation<>b.relationship''',
    'owned_item_ids_and_placement': f'''SELECT COUNT(*) FROM {s}.items a LEFT JOIN {t}.items b ON a.id=b.id WHERE a.user_id>0
        AND (b.id IS NULL OR a.user_id<>b.user_id OR GREATEST(a.room_id,0)<>b.room_id OR a.x<>b.x OR a.y<>b.y OR ABS(a.z-b.z)>0.000000001 OR a.rot<>b.rot)''',
    'bot_coordinates': f'''SELECT COUNT(*) FROM {s}.bots a LEFT JOIN {t}.bots b ON a.id=b.id
        WHERE b.id IS NULL OR a.x<>b.x OR a.y<>b.y OR ABS(a.z-b.z)>0.000000001 OR a.rot<>b.rotation''',
    'pet_coordinates': f'''SELECT COUNT(*) FROM {s}.users_pets a LEFT JOIN {t}.bots b ON b.id=a.id+(SELECT COALESCE(MAX(id),0) FROM {s}.bots)
        WHERE b.id IS NULL OR a.x<>b.x OR a.y<>b.y OR ABS(a.z-b.z)>0.000000001 OR a.rot<>b.rotation''',
    'supported_achievement_totals': f'''SELECT COUNT(*) FROM {s}.users_achievements a JOIN {t}.user_achievements b
        ON b.userid=a.user_id AND b.`group`=IF(LEFT(a.achievement_name,4)='ACH_',a.achievement_name,CONCAT('ACH_',a.achievement_name)) WHERE a.progress<>b.progress''',
    'private_log_count': f'SELECT ABS((SELECT COUNT(*) FROM {s}.chatlogs_private)-(SELECT COUNT(*) FROM {t}.chatlogs_console))',
    'private_log_contents': f'''SELECT COUNT(*) FROM {s}.chatlogs_private a LEFT JOIN {t}.chatlogs_console b ON a.id=b.id
        WHERE b.id IS NULL OR a.user_from_id<>b.from_id OR a.user_to_id<>b.to_id OR BINARY CONVERT(a.message USING utf8mb4)<>BINARY b.message OR a.timestamp<>UNIX_TIMESTAMP(b.timestamp)''',
    'crafting_recipe_count': f'SELECT ABS((SELECT COUNT(*) FROM {s}.crafting_recipes)-(SELECT COUNT(*) FROM {t}.crafting_recipes))',
    'wired_schema': f'SELECT COUNT(*) FROM {t}.wired_item_configurations WHERE schema_version<>2 OR JSON_VALUE(configuration,"$.Version")<>2',
}
results = {name: scalar(sql) for name, sql in checks.items()}
print(json.dumps({'discrepancies': results, 'all_passed': not any(results.values())}, indent=2))
raise SystemExit(1 if any(results.values()) else 0)
