WITH RECURSIVE alias_people(person_id,path) AS (
SELECT $3::uuid,ARRAY[$3::uuid]
UNION ALL
SELECT red.source_person_id,ap.path || red.source_person_id
FROM alias_people ap JOIN litecr.person_redirect red
ON red.registry_id=$1 AND red.organization_id=$2 AND red.target_person_id=ap.person_id AND red.active=TRUE
WHERE NOT red.source_person_id=ANY(ap.path)
) SELECT DISTINCT a.alias_value FROM alias_people ap JOIN litecr.person_alias a
ON a.registry_id=$1 AND a.organization_id=$2 AND a.person_id=ap.person_id ORDER BY a.alias_value;
