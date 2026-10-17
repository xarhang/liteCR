-- Immutable, invoker-only scalar helpers for a conservative retrieval superset.
-- No source data reads, identity mutations, transliteration or extension install.
CREATE FUNCTION matching.name_edit_within_v1(a TEXT, b TEXT, k INTEGER)
RETURNS BOOLEAN LANGUAGE plpgsql IMMUTABLE STRICT PARALLEL SAFE
SET search_path = pg_catalog AS $$
DECLARE
    n INTEGER := char_length(a); m INTEGER := char_length(b);
    prev2 INTEGER[]; prev INTEGER[]; curr INTEGER[];
    i INTEGER; j INTEGER; inf INTEGER := k + 1;
BEGIN
    IF k NOT BETWEEN 0 AND 2 OR n NOT BETWEEN 1 AND 128 OR m NOT BETWEEN 1 AND 128
       OR abs(n-m)>k THEN RETURN FALSE; END IF;
    prev2 := array_fill(inf, ARRAY[m+1], ARRAY[0]);
    prev := array_fill(inf, ARRAY[m+1], ARRAY[0]);
    FOR j IN 0..m LOOP prev[j] := least(j,inf); END LOOP;
    FOR i IN 1..n LOOP
        curr := array_fill(inf, ARRAY[m+1], ARRAY[0]);
        curr[0] := least(i,inf);
        FOR j IN greatest(1,i-k)..least(m,i+k) LOOP
            curr[j] := least(prev[j]+1,curr[j-1]+1,
                prev[j-1]+CASE WHEN substr(a,i,1)=substr(b,j,1) THEN 0 ELSE 1 END);
            IF i>1 AND j>1 AND substr(a,i,1)=substr(b,j-1,1) AND substr(a,i-1,1)=substr(b,j,1)
            THEN curr[j] := least(curr[j],prev2[j-2]+1); END IF;
        END LOOP;
        prev2 := prev; prev := curr;
    END LOOP;
    RETURN prev[m]<=k;
END $$;

CREATE FUNCTION matching.name_prefix_within_v1(a TEXT, b TEXT)
RETURNS BOOLEAN LANGUAGE plpgsql IMMUTABLE STRICT PARALLEL SAFE
SET search_path = pg_catalog AS $$
DECLARE n INTEGER := char_length(a); m INTEGER := char_length(b); j INTEGER;
BEGIN
    IF n NOT BETWEEN 3 AND 128 OR m NOT BETWEEN 3 AND 128 THEN RETURN FALSE; END IF;
    IF starts_with(b,a) THEN RETURN TRUE; END IF;
    IF n<4 THEN RETURN FALSE; END IF;
    FOR j IN greatest(1,n-1)..least(m,n+1) LOOP
        IF matching.name_edit_within_v1(a,left(b,j),1) THEN RETURN TRUE; END IF;
    END LOOP;
    RETURN FALSE;
END $$;

-- Match every supplied name part to a distinct full-name part. Using only one
-- common token would flood the candidate cap with unrelated full names.
CREATE FUNCTION matching.name_parts_within_v1(a TEXT, b TEXT)
RETURNS BOOLEAN LANGUAGE plpgsql IMMUTABLE STRICT PARALLEL SAFE
SET search_path = pg_catalog AS $$
DECLARE
    short TEXT[] := string_to_array(a,' '); full_name TEXT[] := string_to_array(b,' ');
    n INTEGER := cardinality(short); m INTEGER := cardinality(full_name);
    edges BOOLEAN[][]; states INTEGER[] := ARRAY[0]; next_states INTEGER[];
    seen BOOLEAN[]; i INTEGER; j INTEGER; mask INTEGER; new_mask INTEGER;
BEGIN
    IF char_length(a) NOT BETWEEN 4 AND 128 OR char_length(b) NOT BETWEEN 4 AND 128
       OR n NOT BETWEEN 1 AND 8 OR m NOT BETWEEN 1 AND 8 OR n>m THEN RETURN FALSE; END IF;
    edges := array_fill(FALSE, ARRAY[n,m]);
    FOR i IN 1..n LOOP
        IF char_length(short[i])<3 THEN RETURN FALSE; END IF;
        FOR j IN 1..m LOOP
            edges[i][j] := matching.name_prefix_within_v1(short[i],full_name[j]);
        END LOOP;
    END LOOP;
    FOR i IN 1..n LOOP
        next_states := ARRAY[]::INTEGER[];
        seen := array_fill(FALSE, ARRAY[256], ARRAY[0]);
        FOREACH mask IN ARRAY states LOOP
            FOR j IN 1..m LOOP
                IF edges[i][j] AND (mask & (1 << (j-1)))=0 THEN
                    new_mask := mask | (1 << (j-1));
                    IF NOT seen[new_mask] THEN
                        seen[new_mask] := TRUE;
                        next_states := array_append(next_states,new_mask);
                    END IF;
                END IF;
            END LOOP;
        END LOOP;
        IF cardinality(next_states)=0 THEN RETURN FALSE; END IF;
        states := next_states;
    END LOOP;
    RETURN TRUE;
END $$;

DO $$
DECLARE updated_rows INTEGER;
BEGIN
    UPDATE litecr.schema_meta SET version=27, applied_at=now()
    WHERE singleton=TRUE AND version=26;
    GET DIAGNOSTICS updated_rows=ROW_COUNT;
    IF updated_rows<>1 THEN
        RAISE EXCEPTION 'liteCR schema version must be 26 before applying bounded name retrieval migration';
    END IF;
END $$;
