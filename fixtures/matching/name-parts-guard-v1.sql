CASE WHEN NOT EXISTS (SELECT 1 FROM unnest(string_to_array(a,' ')) sp(value)
                     WHERE char_length(sp.value)<3 OR NOT EXISTS (
                       SELECT 1 FROM unnest(string_to_array(b,' ')) fp(value)
                       WHERE char_length(fp.value)>=greatest(3,char_length(sp.value)-1)
                         AND char_length(translate(sp.value,left(fp.value,char_length(sp.value)+1),''))<=1))
                   THEN TRUE ELSE FALSE END