

















































































































































































































































































































































































































































































































































































































































































































































































































































































































































































































































































































































































    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct VecVisitor<T> {
            marker: PhantomData<T>,
        }

        impl<'de, T> Visitor<'de> for VecVisitor<T>
        where
            T: Deserialize<'de>,
        {
            type Value = Vec<T>;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a sequence")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let capacity = size_hint::cautious::<T>(seq.size_hint());
                let mut values = Vec::<T>::with_capacity(capacity);

                while let Some(value) = tri!(seq.next_element()) {
                    values.push(value);
                }

                Ok(values)
            }
        }

        let visitor = VecVisitor {
            marker: PhantomData,
        };
        deserializer.deserialize_seq(visitor)
    }

    fn deserialize_in_place<D>(deserializer: D, place: &mut Self) -> Result<(), D::Error>
    where
        D: Deserializer<'de>,
    {
        struct VecInPlaceVisitor<'a, T: 'a>(&'a mut Vec<T>);

        impl<'a, 'de, T> Visitor<'de> for VecInPlaceVisitor<'a, T>
        where
            T: Deserialize<'de>,
        {
            type Value = ();

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a sequence")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let hint = size_hint::cautious::<T>(seq.size_hint());
                if let Some(additional) = hint.checked_sub(self.0.len()) {
                    self.0.reserve(additional);
                }

                for i in 0..self.0.len() {
                    let next = {
                        let next_place = InPlaceSeed(&mut self.0[i]);
                        tri!(seq.next_element_seed(next_place))
                    };
                    if next.is_none() {
                        self.0.truncate(i);
                        return Ok(());
                    }
                }

                while let Some(value) = tri!(seq.next_element()) {
                    self.0.push(value);
                }

                Ok(())
            }
        }

        deserializer.deserialize_seq(VecInPlaceVisitor(place))
    }










































































































































































































































































































































































































































































































































































































































































































































































































































































































































    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // If this were outside of the serde crate, it would just use:
        //
        //    #[derive(Deserialize)]
        //    #[serde(field_identifier, rename_all = "lowercase")]
        enum Field {
            Secs,
            Nanos,
        }

        impl<'de> Deserialize<'de> for Field {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                struct FieldVisitor;

                impl<'de> Visitor<'de> for FieldVisitor {
                    type Value = Field;

                    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                        formatter.write_str("`secs` or `nanos`")
                    }

                    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
                    where
                        E: Error,
                    {
                        match value {
                            "secs" => Ok(Field::Secs),
                            "nanos" => Ok(Field::Nanos),
                            _ => Err(Error::unknown_field(value, FIELDS)),
                        }
                    }

                    fn visit_bytes<E>(self, value: &[u8]) -> Result<Self::Value, E>
                    where
                        E: Error,
                    {
                        match value {
                            b"secs" => Ok(Field::Secs),
                            b"nanos" => Ok(Field::Nanos),
                            _ => {
                                let value = private::string::from_utf8_lossy(value);
                                Err(Error::unknown_field(&*value, FIELDS))
                            }
                        }
                    }
                }

                deserializer.deserialize_identifier(FieldVisitor)
            }
        }

        fn check_overflow<E>(secs: u64, nanos: u32) -> Result<(), E>
        where
            E: Error,
        {
            static NANOS_PER_SEC: u32 = 1_000_000_000;
            match secs.checked_add((nanos / NANOS_PER_SEC) as u64) {
                Some(_) => Ok(()),
                None => Err(E::custom("overflow deserializing Duration")),
            }
        }

        struct DurationVisitor;

        impl<'de> Visitor<'de> for DurationVisitor {
            type Value = Duration;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("struct Duration")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let secs: u64 = match tri!(seq.next_element()) {
                    Some(value) => value,
                    None => {
                        return Err(Error::invalid_length(0, &self));
                    }
                };
                let nanos: u32 = match tri!(seq.next_element()) {
                    Some(value) => value,
                    None => {
                        return Err(Error::invalid_length(1, &self));
                    }
                };
                tri!(check_overflow(secs, nanos));
                Ok(Duration::new(secs, nanos))
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut secs: Option<u64> = None;
                let mut nanos: Option<u32> = None;
                while let Some(key) = tri!(map.next_key()) {
                    match key {
                        Field::Secs => {
                            if secs.is_some() {
                                return Err(<A::Error as Error>::duplicate_field("secs"));
                            }
                            secs = Some(tri!(map.next_value()));
                        }
                        Field::Nanos => {
                            if nanos.is_some() {
                                return Err(<A::Error as Error>::duplicate_field("nanos"));
                            }
                            nanos = Some(tri!(map.next_value()));
                        }
                    }
                }
                let secs = match secs {
                    Some(secs) => secs,
                    None => return Err(<A::Error as Error>::missing_field("secs")),
                };
                let nanos = match nanos {
                    Some(nanos) => nanos,
                    None => return Err(<A::Error as Error>::missing_field("nanos")),
                };
                tri!(check_overflow(secs, nanos));
                Ok(Duration::new(secs, nanos))
            }
        }

        const FIELDS: &[&str] = &["secs", "nanos"];
        deserializer.deserialize_struct("Duration", FIELDS, DurationVisitor)
    }







    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Reuse duration
        enum Field {
            Secs,
            Nanos,
        }

        impl<'de> Deserialize<'de> for Field {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                struct FieldVisitor;

                impl<'de> Visitor<'de> for FieldVisitor {
                    type Value = Field;

                    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                        formatter.write_str("`secs_since_epoch` or `nanos_since_epoch`")
                    }

                    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
                    where
                        E: Error,
                    {
                        match value {
                            "secs_since_epoch" => Ok(Field::Secs),
                            "nanos_since_epoch" => Ok(Field::Nanos),
                            _ => Err(Error::unknown_field(value, FIELDS)),
                        }
                    }

                    fn visit_bytes<E>(self, value: &[u8]) -> Result<Self::Value, E>
                    where
                        E: Error,
                    {
                        match value {
                            b"secs_since_epoch" => Ok(Field::Secs),
                            b"nanos_since_epoch" => Ok(Field::Nanos),
                            _ => {
                                let value = String::from_utf8_lossy(value);
                                Err(Error::unknown_field(&value, FIELDS))
                            }
                        }
                    }
                }

                deserializer.deserialize_identifier(FieldVisitor)
            }
        }

        fn check_overflow<E>(secs: u64, nanos: u32) -> Result<(), E>
        where
            E: Error,
        {
            static NANOS_PER_SEC: u32 = 1_000_000_000;
            match secs.checked_add((nanos / NANOS_PER_SEC) as u64) {
                Some(_) => Ok(()),
                None => Err(E::custom("overflow deserializing SystemTime epoch offset")),
            }
        }

        struct DurationVisitor;

        impl<'de> Visitor<'de> for DurationVisitor {
            type Value = Duration;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("struct SystemTime")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let secs: u64 = match tri!(seq.next_element()) {
                    Some(value) => value,
                    None => {
                        return Err(Error::invalid_length(0, &self));
                    }
                };
                let nanos: u32 = match tri!(seq.next_element()) {
                    Some(value) => value,
                    None => {
                        return Err(Error::invalid_length(1, &self));
                    }
                };
                tri!(check_overflow(secs, nanos));
                Ok(Duration::new(secs, nanos))
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut secs: Option<u64> = None;
                let mut nanos: Option<u32> = None;
                while let Some(key) = tri!(map.next_key()) {
                    match key {
                        Field::Secs => {
                            if secs.is_some() {
                                return Err(<A::Error as Error>::duplicate_field(
                                    "secs_since_epoch",
                                ));
                            }
                            secs = Some(tri!(map.next_value()));
                        }
                        Field::Nanos => {
                            if nanos.is_some() {
                                return Err(<A::Error as Error>::duplicate_field(
                                    "nanos_since_epoch",
                                ));
                            }
                            nanos = Some(tri!(map.next_value()));
                        }
                    }
                }
                let secs = match secs {
                    Some(secs) => secs,
                    None => return Err(<A::Error as Error>::missing_field("secs_since_epoch")),
                };
                let nanos = match nanos {
                    Some(nanos) => nanos,
                    None => return Err(<A::Error as Error>::missing_field("nanos_since_epoch")),
                };
                tri!(check_overflow(secs, nanos));
                Ok(Duration::new(secs, nanos))
            }
        }

        const FIELDS: &[&str] = &["secs_since_epoch", "nanos_since_epoch"];
        let duration = tri!(deserializer.deserialize_struct("SystemTime", FIELDS, DurationVisitor));
        UNIX_EPOCH
            .checked_add(duration)
            .ok_or_else(|| D::Error::custom("overflow deserializing SystemTime"))
    }






































































        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            struct FieldVisitor;

            impl<'de> Visitor<'de> for FieldVisitor {
                type Value = Field;

                fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                    formatter.write_str("`start` or `end`")
                }

                fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
                where
                    E: Error,
                {
                    match value {
                        "start" => Ok(Field::Start),
                        "end" => Ok(Field::End),
                        _ => Err(Error::unknown_field(value, FIELDS)),
                    }
                }

                fn visit_bytes<E>(self, value: &[u8]) -> Result<Self::Value, E>
                where
                    E: Error,
                {
                    match value {
                        b"start" => Ok(Field::Start),
                        b"end" => Ok(Field::End),
                        _ => {
                            let value = private::string::from_utf8_lossy(value);
                            Err(Error::unknown_field(&*value, FIELDS))
                        }
                    }
                }
            }

            deserializer.deserialize_identifier(FieldVisitor)
        }





















































































































        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            struct FieldVisitor;

            impl<'de> Visitor<'de> for FieldVisitor {
                type Value = Field;

                fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                    formatter.write_str("`start`")
                }

                fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
                where
                    E: Error,
                {
                    match value {
                        "start" => Ok(Field::Start),
                        _ => Err(Error::unknown_field(value, FIELDS)),
                    }
                }

                fn visit_bytes<E>(self, value: &[u8]) -> Result<Self::Value, E>
                where
                    E: Error,
                {
                    match value {
                        b"start" => Ok(Field::Start),
                        _ => {
                            let value = private::string::from_utf8_lossy(value);
                            Err(Error::unknown_field(&*value, FIELDS))
                        }
                    }
                }
            }

            deserializer.deserialize_identifier(FieldVisitor)
        }




































































































        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            struct FieldVisitor;

            impl<'de> Visitor<'de> for FieldVisitor {
                type Value = Field;

                fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                    formatter.write_str("`end`")
                }

                fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
                where
                    E: Error,
                {
                    match value {
                        "end" => Ok(Field::End),
                        _ => Err(Error::unknown_field(value, FIELDS)),
                    }
                }

                fn visit_bytes<E>(self, value: &[u8]) -> Result<Self::Value, E>
                where
                    E: Error,
                {
                    match value {
                        b"end" => Ok(Field::End),
                        _ => {
                            let value = private::string::from_utf8_lossy(value);
                            Err(Error::unknown_field(&*value, FIELDS))
                        }
                    }
                }
            }

            deserializer.deserialize_identifier(FieldVisitor)
        }




























































    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        enum Field {
            Unbounded,
            Included,
            Excluded,
        }

        impl<'de> Deserialize<'de> for Field {
            #[inline]
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                struct FieldVisitor;

                impl<'de> Visitor<'de> for FieldVisitor {
                    type Value = Field;

                    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                        formatter.write_str("`Unbounded`, `Included` or `Excluded`")
                    }

                    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
                    where
                        E: Error,
                    {
                        match value {
                            0 => Ok(Field::Unbounded),
                            1 => Ok(Field::Included),
                            2 => Ok(Field::Excluded),
                            _ => Err(Error::invalid_value(Unexpected::Unsigned(value), &self)),
                        }
                    }

                    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
                    where
                        E: Error,
                    {
                        match value {
                            "Unbounded" => Ok(Field::Unbounded),
                            "Included" => Ok(Field::Included),
                            "Excluded" => Ok(Field::Excluded),
                            _ => Err(Error::unknown_variant(value, VARIANTS)),
                        }
                    }

                    fn visit_bytes<E>(self, value: &[u8]) -> Result<Self::Value, E>
                    where
                        E: Error,
                    {
                        match value {
                            b"Unbounded" => Ok(Field::Unbounded),
                            b"Included" => Ok(Field::Included),
                            b"Excluded" => Ok(Field::Excluded),
                            _ => match str::from_utf8(value) {
                                Ok(value) => Err(Error::unknown_variant(value, VARIANTS)),
                                Err(_) => {
                                    Err(Error::invalid_value(Unexpected::Bytes(value), &self))
                                }
                            },
                        }
                    }
                }

                deserializer.deserialize_identifier(FieldVisitor)
            }
        }

        struct BoundVisitor<T>(PhantomData<Bound<T>>);

        impl<'de, T> Visitor<'de> for BoundVisitor<T>
        where
            T: Deserialize<'de>,
        {
            type Value = Bound<T>;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("enum Bound")
            }

            fn visit_enum<A>(self, data: A) -> Result<Self::Value, A::Error>
            where
                A: EnumAccess<'de>,
            {
                match tri!(data.variant()) {
                    (Field::Unbounded, v) => v.unit_variant().map(|()| Bound::Unbounded),
                    (Field::Included, v) => v.newtype_variant().map(Bound::Included),
                    (Field::Excluded, v) => v.newtype_variant().map(Bound::Excluded),
                }
            }
        }

        const VARIANTS: &[&str] = &["Unbounded", "Included", "Excluded"];

        deserializer.deserialize_enum("Bound", VARIANTS, BoundVisitor(PhantomData))
    }











    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // If this were outside of the serde crate, it would just use:
        //
        //    #[derive(Deserialize)]
        //    #[serde(variant_identifier)]
        enum Field {
            Ok,
            Err,
        }

        impl<'de> Deserialize<'de> for Field {
            #[inline]
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                struct FieldVisitor;

                impl<'de> Visitor<'de> for FieldVisitor {
                    type Value = Field;

                    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                        formatter.write_str("`Ok` or `Err`")
                    }

                    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
                    where
                        E: Error,
                    {
                        match value {
                            0 => Ok(Field::Ok),
                            1 => Ok(Field::Err),
                            _ => Err(Error::invalid_value(Unexpected::Unsigned(value), &self)),
                        }
                    }

                    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
                    where
                        E: Error,
                    {
                        match value {
                            "Ok" => Ok(Field::Ok),
                            "Err" => Ok(Field::Err),
                            _ => Err(Error::unknown_variant(value, VARIANTS)),
                        }
                    }

                    fn visit_bytes<E>(self, value: &[u8]) -> Result<Self::Value, E>
                    where
                        E: Error,
                    {
                        match value {
                            b"Ok" => Ok(Field::Ok),
                            b"Err" => Ok(Field::Err),
                            _ => match str::from_utf8(value) {
                                Ok(value) => Err(Error::unknown_variant(value, VARIANTS)),
                                Err(_) => {
                                    Err(Error::invalid_value(Unexpected::Bytes(value), &self))
                                }
                            },
                        }
                    }
                }

                deserializer.deserialize_identifier(FieldVisitor)
            }
        }

        struct ResultVisitor<T, E>(PhantomData<Result<T, E>>);

        impl<'de, T, E> Visitor<'de> for ResultVisitor<T, E>
        where
            T: Deserialize<'de>,
            E: Deserialize<'de>,
        {
            type Value = Result<T, E>;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("enum Result")
            }

            fn visit_enum<A>(self, data: A) -> Result<Self::Value, A::Error>
            where
                A: EnumAccess<'de>,
            {
                match tri!(data.variant()) {
                    (Field::Ok, v) => v.newtype_variant().map(Ok),
                    (Field::Err, v) => v.newtype_variant().map(Err),
                }
            }
        }

        const VARIANTS: &[&str] = &["Ok", "Err"];

        deserializer.deserialize_enum("Result", VARIANTS, ResultVisitor(PhantomData))
    }
























































































