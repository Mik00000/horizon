//! A viewer's transfer of as many sets as the items allow (`Menu::transfer_recipe`
//! with `all`), against the vanilla data the menu tests use.
use crate::menu::{
    Kind, Menu,
    tests::{mod_crafting_table, stack, with_menu},
};

/// A viewer's transfer with `all` (Shift on JEI's "+") fills as many sets
/// as the items allow, as the recipe book's Shift placement does: the
/// largest count every grid slot can take, one item kind per slot, capped
/// at that item's stack size, the rest left where it was.
#[test]
fn a_transfer_of_all_fills_as_many_sets_as_the_items_allow() {
    let grid = |m: &Menu, slots: std::ops::Range<usize>| {
        slots
            .map(|s| m.slot(s).map(|s| (s.item.to_string(), s.count)))
            .collect::<Vec<_>>()
    };
    let inventory = |m: &Menu| {
        (0..36)
            .filter_map(|i| m.player_item(i).map(|s| (s.item.to_string(), s.count)))
            .collect::<Vec<_>>()
    };
    let some = |item: &str, n: u32| Some((format!("minecraft:{item}"), n));
    // Planks from one log, in the inventory's grid: every log up to a stack.
    with_menu(Kind::Inventory, |env, menu| {
        menu.set_player_item(0, stack("oak_log", 64));
        menu.set_player_item(1, stack("oak_log", 36));
        assert_eq!(
            menu.transfer_recipe(env, "minecraft:oak_planks", true),
            Ok(())
        );
        let filled: Vec<_> = grid(menu, 1..5).into_iter().flatten().collect();
        assert_eq!(filled, [some("oak_log", 64).unwrap()]);
        assert_eq!(inventory(menu), [some("oak_log", 36).unwrap()]);
    });
    with_menu(Kind::Inventory, |env, menu| {
        menu.set_player_item(3, stack("oak_log", 40));
        assert_eq!(
            menu.transfer_recipe(env, "minecraft:oak_planks", true),
            Ok(())
        );
        let filled: Vec<_> = grid(menu, 1..5).into_iter().flatten().collect();
        assert_eq!(filled, [some("oak_log", 40).unwrap()]);
        assert_eq!(inventory(menu), []);
    });
    // Slabs, three planks in a row, from 14 planks: 4 crafts, 2 planks left.
    // Without `all`, one set.
    with_menu(Kind::Crafting, |env, menu| {
        menu.set_player_item(0, stack("oak_planks", 14));
        assert_eq!(
            menu.transfer_recipe(env, "minecraft:oak_slab", false),
            Ok(())
        );
        let one: Vec<_> = grid(menu, 1..10).into_iter().flatten().collect();
        assert_eq!(one, vec![some("oak_planks", 1).unwrap(); 3]);
        assert_eq!(
            menu.transfer_recipe(env, "minecraft:oak_slab", true),
            Ok(())
        );
        let all: Vec<_> = grid(menu, 1..10).into_iter().flatten().collect();
        assert_eq!(all, vec![some("oak_planks", 4).unwrap(); 3]);
        assert_eq!(inventory(menu), [some("oak_planks", 2).unwrap()]);
    });
    // A tag shared by two slots: sticks from 3 oak and 3 birch planks put
    // one kind in each slot, 3 each.
    with_menu(Kind::Inventory, |env, menu| {
        menu.set_player_item(0, stack("oak_planks", 3));
        menu.set_player_item(1, stack("birch_planks", 3));
        assert_eq!(menu.transfer_recipe(env, "minecraft:stick", true), Ok(()));
        let mut filled: Vec<_> = grid(menu, 1..5).into_iter().flatten().collect();
        filled.sort();
        assert_eq!(
            filled,
            [
                some("birch_planks", 3).unwrap(),
                some("oak_planks", 3).unwrap()
            ]
        );
        assert_eq!(inventory(menu), []);
    });
    // Four slots of any planks from 5 oak and 3 birch: two of each in every
    // slot would need two slots of birch, so one set.
    with_menu(Kind::Inventory, |env, menu| {
        menu.set_player_item(0, stack("oak_planks", 5));
        menu.set_player_item(1, stack("birch_planks", 3));
        assert_eq!(
            menu.transfer_recipe(env, "minecraft:crafting_table", true),
            Ok(())
        );
        let counts: Vec<_> = grid(menu, 1..5)
            .into_iter()
            .flatten()
            .map(|(_, n)| n)
            .collect();
        assert_eq!(counts, [1, 1, 1, 1]);
    });
    // Snowballs stack to 16: a snow block's four slots stop there, though
    // 100 snowballs would give 25 each.
    with_menu(Kind::Inventory, |env, menu| {
        for i in 0..6 {
            menu.set_player_item(i, stack("snowball", 16));
        }
        menu.set_player_item(6, stack("snowball", 4));
        assert_eq!(
            menu.transfer_recipe(env, "minecraft:snow_block", true),
            Ok(())
        );
        let filled: Vec<_> = grid(menu, 1..5).into_iter().flatten().collect();
        assert_eq!(filled, vec![some("snowball", 16).unwrap(); 4]);
        let left: u32 = inventory(menu).iter().map(|(_, n)| n).sum();
        assert_eq!(left, 100 - 64);
    });
    // A mod crafting grid counts its sources: 2 planks in hand and 12 in a
    // chest beside it make the same 4 slabs, the hand's taken first.
    with_menu(mod_crafting_table(), |env, station| {
        station.set_player_item(0, stack("oak_planks", 2));
        station.sources = vec![vec![stack("oak_planks", 12)]];
        assert_eq!(
            station.transfer_recipe(env, "minecraft:oak_slab", true),
            Ok(())
        );
        let all: Vec<_> = grid(station, 1..10).into_iter().flatten().collect();
        assert_eq!(all, vec![some("oak_planks", 4).unwrap(); 3]);
        assert_eq!(station.player_item(0), None);
        assert_eq!(station.sources, vec![vec![stack("oak_planks", 2)]]);
    });
}

/// A viewer's transfer moves worn, enchanted or renamed tools that match the recipe
/// ingredients (#14652), while the vanilla recipe book skips them (`PostPlace::Ghost`).
#[test]
fn recipe_viewer_transfer_allows_worn_enchanted_and_renamed_tools() {
    // 1. Worn fishing rod (damage > 0) with a carrot into carrot_on_a_stick
    with_menu(Kind::Inventory, |env, menu| {
        let mut rod = stack("fishing_rod", 1).unwrap();
        rod.damage = 25;
        menu.set_player_item(0, Some(rod));
        menu.set_player_item(1, stack("carrot", 1));

        // Recipe book skips worn tool: leaves grid untouched and returns Ghost
        let placed = menu.place_recipe(env, "minecraft:carrot_on_a_stick", false, false, None);
        assert_eq!(placed, crate::menu::place::PostPlace::Ghost);
        assert_eq!(menu.slot(1), None);
        assert_eq!(menu.slot(4), None);

        // Recipe viewer transfer succeeds with worn tool
        assert_eq!(
            menu.transfer_recipe(env, "minecraft:carrot_on_a_stick", false),
            Ok(())
        );
        let rod_slot = menu.slot(1).map(|s| (s.item.to_string(), s.damage));
        let carrot_slot = menu.slot(4).map(|s| (s.item.to_string(), s.count));
        assert_eq!(rod_slot, Some(("minecraft:fishing_rod".to_string(), 25)));
        assert_eq!(carrot_slot, Some(("minecraft:carrot".to_string(), 1)));
        assert_eq!(
            menu.slot(0).map(|s| s.item.to_string()),
            Some("minecraft:carrot_on_a_stick".to_string())
        );
    });

    // 2. Enchanted fishing rod with a carrot
    with_menu(Kind::Inventory, |env, menu| {
        let mut rod = stack("fishing_rod", 1).unwrap();
        rod.data_mut().enchantments = vec![("minecraft:unbreaking".into(), 2)];
        menu.set_player_item(0, Some(rod));
        menu.set_player_item(1, stack("carrot", 1));

        // Recipe book skips enchanted tool
        let placed = menu.place_recipe(env, "minecraft:carrot_on_a_stick", false, false, None);
        assert_eq!(placed, crate::menu::place::PostPlace::Ghost);
        assert_eq!(menu.slot(1), None);
        assert_eq!(menu.slot(4), None);

        // Recipe viewer transfer accepts enchanted tool
        assert_eq!(
            menu.transfer_recipe(env, "minecraft:carrot_on_a_stick", false),
            Ok(())
        );
        assert!(menu.slot(1).is_some_and(|s| s.enchantments().len() == 1));
        assert_eq!(
            menu.slot(0).map(|s| s.item.to_string()),
            Some("minecraft:carrot_on_a_stick".to_string())
        );
    });

    // 3. Renamed fishing rod with a carrot
    with_menu(Kind::Inventory, |env, menu| {
        let mut rod = stack("fishing_rod", 1).unwrap();
        rod.data_mut().custom_name = Some(horizon_world::nbt::Tag::String("Old Rod".into()));
        menu.set_player_item(0, Some(rod));
        menu.set_player_item(1, stack("carrot", 1));

        // Recipe book skips renamed tool
        let placed = menu.place_recipe(env, "minecraft:carrot_on_a_stick", false, false, None);
        assert_eq!(placed, crate::menu::place::PostPlace::Ghost);
        assert_eq!(menu.slot(1), None);
        assert_eq!(menu.slot(4), None);

        // Recipe viewer transfer accepts renamed tool
        assert_eq!(
            menu.transfer_recipe(env, "minecraft:carrot_on_a_stick", false),
            Ok(())
        );
        assert_eq!(
            menu.slot(1).and_then(|s| s.custom_name()),
            Some("Old Rod".to_string())
        );
        assert_eq!(
            menu.slot(0).map(|s| s.item.to_string()),
            Some("minecraft:carrot_on_a_stick".to_string())
        );
    });

    // 4. Transfer with `all = true` also works with worn tools
    with_menu(Kind::Crafting, |env, menu| {
        let mut rod = stack("fishing_rod", 1).unwrap();
        rod.damage = 10;
        menu.set_player_item(0, Some(rod));
        menu.set_player_item(1, stack("carrot", 5));

        assert_eq!(
            menu.transfer_recipe(env, "minecraft:carrot_on_a_stick", true),
            Ok(())
        );
        assert!(menu.slot(1).is_some_and(|s| s.damage == 10 && s.count == 1));
        assert_eq!(menu.slot(5).map(|s| s.count), Some(1));
        assert_eq!(
            menu.slot(0).map(|s| s.item.to_string()),
            Some("minecraft:carrot_on_a_stick".to_string())
        );
        // Remaining carrots in inventory
        assert_eq!(menu.player_item(1).map(|s| s.count), Some(4));
    });
}
