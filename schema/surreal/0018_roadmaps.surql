BEGIN TRANSACTION;
-- In-app roadmaps: the owner's plans, their items, an audit of every change
-- and the changes Jarvis proposes. Core writes them only through typed store
-- functions; nothing is seeded. Additive: no existing table changes.

-- One roadmap. `revision` grows by one on every change and guards
-- concurrent edits. Archived instead of deleted.
DEFINE TABLE IF NOT EXISTS roadmaps SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON roadmaps TYPE string;
DEFINE FIELD title ON roadmaps TYPE string ASSERT string::len($value) >= 1 AND string::len($value) <= 120;
DEFINE FIELD description ON roadmaps TYPE option<string> ASSERT $value = NONE OR (string::len($value) >= 1 AND string::len($value) <= 2000);
DEFINE FIELD phases ON roadmaps TYPE array<string, 20> ASSERT array::len(array::distinct($value)) = array::len($value);
DEFINE FIELD OVERWRITE phases.* ON roadmaps TYPE string ASSERT string::len($value) >= 1 AND string::len($value) <= 60;
DEFINE FIELD revision ON roadmaps TYPE int ASSERT $value >= 1;
DEFINE FIELD created_at ON roadmaps TYPE datetime;
DEFINE FIELD updated_at ON roadmaps TYPE datetime;
DEFINE FIELD archived_at ON roadmaps TYPE option<datetime>;
DEFINE INDEX IF NOT EXISTS roadmaps_id ON roadmaps FIELDS id UNIQUE;

-- The items of a roadmap. `depends_on` names items of the same roadmap
-- (Core checks that and refuses cycles). `auto_status` is the status the
-- GitHub poller last derived and a link's `state` the GitHub state it last
-- saw; owner edits never write either.
DEFINE TABLE IF NOT EXISTS roadmap_items SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON roadmap_items TYPE string;
DEFINE FIELD roadmap_id ON roadmap_items TYPE string ASSERT $value = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
DEFINE FIELD title ON roadmap_items TYPE string ASSERT string::len($value) >= 1 AND string::len($value) <= 200;
DEFINE FIELD status ON roadmap_items TYPE string ASSERT $value IN ['live', 'ready', 'in_progress', 'planned', 'blocked'];
DEFINE FIELD auto_status ON roadmap_items TYPE option<string> ASSERT $value = NONE OR $value IN ['live', 'ready', 'in_progress', 'planned', 'blocked'];
DEFINE FIELD phase ON roadmap_items TYPE option<string> ASSERT $value = NONE OR (string::len($value) >= 1 AND string::len($value) <= 60);
DEFINE FIELD position ON roadmap_items TYPE int ASSERT $value >= 0 AND $value <= 100000;
DEFINE FIELD notes ON roadmap_items TYPE option<string> ASSERT $value = NONE OR (string::len($value) >= 1 AND string::len($value) <= 4000);
DEFINE FIELD depends_on ON roadmap_items TYPE array<string, 20> ASSERT array::len(array::distinct($value)) = array::len($value);
DEFINE FIELD OVERWRITE depends_on.* ON roadmap_items TYPE string ASSERT $value = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
DEFINE FIELD links ON roadmap_items TYPE array<object, 10>;
DEFINE FIELD links.*.kind ON roadmap_items TYPE string ASSERT $value IN ['pr', 'issue', 'release', 'url'];
DEFINE FIELD links.*.repo ON roadmap_items TYPE option<string> ASSERT $value = NONE OR $value = /^[A-Za-z0-9][A-Za-z0-9-]{0,38}\/[A-Za-z0-9._-]{1,100}$/;
DEFINE FIELD links.*.number ON roadmap_items TYPE option<int> ASSERT $value = NONE OR ($value >= 1 AND $value <= 1000000000);
DEFINE FIELD links.*.tag ON roadmap_items TYPE option<string> ASSERT $value = NONE OR $value = /^[A-Za-z0-9._+\/-]{1,100}$/;
DEFINE FIELD links.*.url ON roadmap_items TYPE option<string> ASSERT $value = NONE OR $value = /^https:\/\/[!-~]{1,492}$/;
DEFINE FIELD links.*.state ON roadmap_items TYPE option<string> ASSERT $value = NONE OR $value = /^[a-z_]{1,32}$/;
DEFINE FIELD created_at ON roadmap_items TYPE datetime;
DEFINE FIELD updated_at ON roadmap_items TYPE datetime;
DEFINE INDEX IF NOT EXISTS roadmap_items_id ON roadmap_items FIELDS id UNIQUE;
DEFINE INDEX IF NOT EXISTS roadmap_items_roadmap ON roadmap_items FIELDS roadmap_id;

-- One row per change, written in the transaction that makes it. The owner
-- changes from a device; a GitHub change has no device; a Jarvis change is
-- an accepted proposal, accepted from a device. Pruned after 365 days.
DEFINE TABLE IF NOT EXISTS roadmap_changes SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON roadmap_changes TYPE string;
DEFINE FIELD roadmap_id ON roadmap_changes TYPE string ASSERT $value = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
DEFINE FIELD revision ON roadmap_changes TYPE int ASSERT $value >= 1;
DEFINE FIELD at ON roadmap_changes TYPE datetime;
DEFINE FIELD author ON roadmap_changes TYPE string ASSERT $value IN ['owner', 'jarvis', 'github'] AND ($value = 'github') = (device_id = NONE) AND ($value = 'jarvis') = (proposal_id != NONE);
DEFINE FIELD device_id ON roadmap_changes TYPE option<string> ASSERT $value = NONE OR $value = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
DEFINE FIELD proposal_id ON roadmap_changes TYPE option<string> ASSERT $value = NONE OR $value = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
DEFINE FIELD action ON roadmap_changes TYPE string ASSERT $value IN ['roadmap_created', 'roadmap_updated', 'roadmap_archived', 'item_created', 'item_updated', 'item_deleted'] AND string::starts_with($value, 'item_') = (item_id != NONE);
DEFINE FIELD item_id ON roadmap_changes TYPE option<string> ASSERT $value = NONE OR $value = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
DEFINE FIELD title ON roadmap_changes TYPE string ASSERT string::len($value) >= 1 AND string::len($value) <= 200;
DEFINE FIELD fields ON roadmap_changes TYPE array<string, 10> ASSERT $value ALLINSIDE ['title', 'description', 'phases', 'status', 'phase', 'position', 'notes', 'depends_on', 'links'];
DEFINE FIELD from_status ON roadmap_changes TYPE option<string> ASSERT $value = NONE OR $value IN ['live', 'ready', 'in_progress', 'planned', 'blocked'];
DEFINE FIELD to_status ON roadmap_changes TYPE option<string> ASSERT $value = NONE OR $value IN ['live', 'ready', 'in_progress', 'planned', 'blocked'];
DEFINE INDEX IF NOT EXISTS roadmap_changes_id ON roadmap_changes FIELDS id UNIQUE;
DEFINE INDEX IF NOT EXISTS roadmap_changes_roadmap ON roadmap_changes FIELDS roadmap_id, revision;
DEFINE INDEX IF NOT EXISTS roadmap_changes_at ON roadmap_changes FIELDS at;

-- Changes Jarvis proposes against one revision: at most 20 operations, each
-- canonical JSON. `rationale` is display only. Pending for 14 days; the
-- owner accepts (applied atomically) or rejects. Pruned 365 days after the
-- decision.
DEFINE TABLE IF NOT EXISTS roadmap_proposals SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON roadmap_proposals TYPE string;
DEFINE FIELD roadmap_id ON roadmap_proposals TYPE string ASSERT $value = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
DEFINE FIELD base_revision ON roadmap_proposals TYPE int ASSERT $value >= 1;
DEFINE FIELD ops ON roadmap_proposals TYPE array<string, 20> ASSERT array::len($value) >= 1;
DEFINE FIELD OVERWRITE ops.* ON roadmap_proposals TYPE string ASSERT string::len($value) >= 2 AND bytes::len(<bytes>$value) <= 24576;
DEFINE FIELD rationale ON roadmap_proposals TYPE option<string> ASSERT $value = NONE OR (string::len($value) >= 1 AND string::len($value) <= 1000);
DEFINE FIELD status ON roadmap_proposals TYPE string ASSERT $value IN ['pending', 'accepted', 'rejected', 'stale', 'expired'] AND ($value = 'pending') = (decided_at = NONE) AND ($value IN ['accepted', 'rejected']) = (decided_by != NONE);
DEFINE FIELD created_at ON roadmap_proposals TYPE datetime;
DEFINE FIELD expires_at ON roadmap_proposals TYPE datetime ASSERT $value > created_at;
DEFINE FIELD decided_at ON roadmap_proposals TYPE option<datetime>;
DEFINE FIELD decided_by ON roadmap_proposals TYPE option<string> ASSERT $value = NONE OR $value = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
DEFINE INDEX IF NOT EXISTS roadmap_proposals_id ON roadmap_proposals FIELDS id UNIQUE;
DEFINE INDEX IF NOT EXISTS roadmap_proposals_roadmap ON roadmap_proposals FIELDS roadmap_id, status;
DEFINE INDEX IF NOT EXISTS roadmap_proposals_decided ON roadmap_proposals FIELDS decided_at;

UPDATE schema_version:baseline SET version = 18, applied_at = time::now();
COMMIT TRANSACTION;
